
// backend/src/process/ffmpeg.rs
//! 构建顺序转码命令并恢复和验证持久 HLS.

use std::{path::Path, process::Stdio};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tokio::{process::Command, sync::watch};
use tracing::warn;

use crate::{models::{CacheSpec, MediaMetadata}, utils::atomic_file};


use super::{
    assets::RenderAssets,
    fragment::{self, Fragment, Init, Tail},
    cancelled, check_cancel, checked_output_cancellable, Cancelled, ProcessManager,
};

#[cfg(test)]
#[path = "../../tests/process/ffmpeg.rs"]
mod tests;



impl ProcessManager {
/// 只向私有工作目录生成 HLS, 正式缓存由句柄校验后原子追加.
pub(crate) fn command(&self, spec: &CacheSpec, source_path: &Path, metadata: &MediaMetadata,
    directory: &Path, assets: Option<&RenderAssets>, resume: Tail, threads: usize) -> Result<Command> {
    let video = metadata.video_streams.iter().find(|stream| stream.stream_index == spec.video_stream_index)
        .context("selected video stream does not exist")?;
    let subtitle = spec.subtitle_stream_index.map(|stream_index|
        metadata.subtitle_streams.iter().find(|stream| stream.stream_index == stream_index)
            .context("selected subtitle stream does not exist")).transpose()?;
    let hdr = video.is_hdr();
    let mut command = Command::new("ffmpeg");
    command.current_dir(directory).kill_on_drop(true)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.args(["-nostdin", "-hide_banner", "-v", "warning", "-xerror", "-y", "-copyts",
        "-threads", &threads.to_string(), "-filter_threads", "1", "-filter_complex_threads", "1"]);
    let seek = (resume.seconds() - 1.0).max(0.0);
    if seek > 0.0 { command.args(["-ss", &format!("{seek:.9}")]); }
    command.arg("-i").arg(source_path);
    let bitmap = subtitle.filter(|stream| stream.is_bitmap());
    let history = usize::from(resume.video > 0 && (spec.audio_stream_index.is_some() || bitmap.is_some()));
    if history == 1 {
        // 音频重采样和位图字幕依赖历史状态. 第二路只解码这些流, 不重编码已有视频.
        command.args(["-threads", "1", "-i"]).arg(source_path);
    }
    let mut filters = Vec::new();
    if hdr {
        filters.push("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv".to_string());
    }
    filters.push("scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2".to_string());
    filters.push("format=yuv420p".to_string());
    if subtitle.is_some_and(|stream| !stream.is_bitmap()) {
        filters.push(assets.context("text subtitle resources are missing")?.filter().to_string());
    }
    // 先按原时间烧录字幕, 再固定有理数帧率和全片采样原点, 避免续接重置选帧相位.
    filters.push(video_timing(metadata, resume));
    if let Some(track) = bitmap {
        command.args(["-filter_complex",
            &format!("[0:{}][{history}:{}]overlay=eof_action=pass,{}[video]",
                spec.video_stream_index, track.stream_index, filters.join(",")),
            "-map", "[video]"]);
    } else {
        command.args(["-map", &format!("0:{}", spec.video_stream_index), "-vf", &filters.join(",")]);
    }
    // 禁用 B 帧以统一显示和解码时间, 定期关键帧保证每个分片可独立解码.
    command.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
        "-pix_fmt", "yuv420p", "-profile:v", "high", "-threads", &threads.to_string(),
        "-bf", "0", "-force_key_frames", "expr:gte(t,n_forced*4)", "-sc_threshold", "0",
        "-fps_mode", "passthrough", "-enc_time_base:v", "1:90000"]);
    if hdr { command.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]); }
    if let Some(index) = spec.audio_stream_index {
        let warmup = resume.audio.min(8192);
        let samples = (metadata.duration * 48_000.0).ceil() as u64;
        // 从固定采样原点裁切, 丢弃 AAC 编码延迟和预热包, 而不是把它们追加到旧音轨.
        let audio = format!("asetpts=PTS-{:.9}/TB,aresample=48000:async=1:first_pts=0,\
            apad=whole_len={samples},atrim=end_sample={samples},atrim=start_sample={},asetpts=(N-{warmup})/SR/TB",
            metadata.start_time, resume.audio - warmup);
        command.args(["-map", &format!("{history}:{index}"), "-af", &audio,
            "-c:a", "aac", "-profile:a", "aac_low", "-ac", "2", "-ar", "48000", "-b:a", "192k",
            "-bsf:a", "noise=drop='lt(pts,0)'"]);
    } else { command.arg("-an"); }
    command.args(["-sn", "-dn", "-map_metadata", "-1", "-avoid_negative_ts", "disabled",
        "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", "4",
        "-hls_list_size", "0", "-hls_playlist_type", "event",
        "-hls_flags", "independent_segments+temp_file",
        "-hls_segment_options", "use_editlist=0:video_track_timescale=90000",
        "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename", "segment_%06d.m4s", "index.m3u8"]);
    Ok(command)
}

/// 用真实解码确认续接点之后是否仍有视频, 不根据估算时长猜测 EOF.
pub(crate) async fn has_video_after(&self, spec: &CacheSpec, source_path: &Path, metadata: &MediaMetadata, resume: Tail,
    cancel: &mut watch::Receiver<bool>) -> Result<bool> {
    let _permit = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(Cancelled.into()),
        permit = self.process_slots.acquire() => permit?,
    };
    // 最后一个分片提交后也可能恰好中断. 用真实解码确认 EOF, 不靠时长容差猜测完成.
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-v", "error", "-xerror", "-copyts", "-threads", "1",
        "-filter_threads", "1", "-ss", &format!("{:.9}", (resume.seconds() - 1.0).max(0.0)), "-i"])
        .arg(source_path).args(["-map", &format!("0:{}", spec.video_stream_index),
            "-vf", &video_timing(metadata, resume), "-frames:v", "1", "-threads:v", "1",
            "-an", "-sn", "-dn", "-f", "framemd5", "-"]);
    let output = checked_output_cancellable(&mut command, 60, cancel).await?;
    let text = std::str::from_utf8(&output)?;
    ensure!(text.starts_with("#format: frame checksums"), "invalid resume frame check output");
    Ok(text.lines().any(|line| !line.is_empty() && !line.starts_with('#')))
}
}

/// 保存磁盘清单中的连续资源和总时长.
#[derive(Clone, Debug)]
pub(crate) struct Playlist {
    /// 从零连续编号的已发布分片名称.
    pub(crate) segments: Vec<String>,
    /// 连续前缀对应的视频秒数.
    pub(crate) duration: f64,
    /// 是否含有最终 ENDLIST 标记.
    pub(crate) complete: bool,
    /// 各分片的声明时长, 用于恢复时校验完整性.
    durations: Vec<f64>,
}

/// 读取已原子发布的清单, 尚未生成时返回 None.
pub(crate) async fn read_playlist(directory: &Path) -> Result<Option<Playlist>> {
    let path = directory.join("index.m3u8");
    let content = match regular_bytes(&path, 16 * 1024 * 1024).await {
        Ok(content) => content,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(parse_playlist(std::str::from_utf8(&content)?)?))
}

/// 仅接受从零连续编号的本地资源, 拒绝外部 URI 和不连续清单.
fn parse_playlist(content: &str) -> Result<Playlist> {
    ensure!(content.starts_with("#EXTM3U\n") && content.contains("#EXT-X-MAP:URI=\"init.mp4\""),
        "invalid fMP4 playlist");
    let mut playlist = Playlist { segments: Vec::new(), durations: Vec::new(), duration: 0.0, complete: false };
    let mut pending = None;
    for line in content.lines().filter(|line| !line.is_empty()) {
        ensure!(!playlist.complete, "data follows the HLS end marker");
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            ensure!(pending.is_none(), "HLS segment URI is missing");
            let duration = value.split(',').next().context("invalid HLS duration")?.parse::<f64>()?;
            ensure!(duration.is_finite() && duration > 0.0, "invalid HLS segment duration");
            pending = Some(duration);
        } else if line == "#EXT-X-ENDLIST" {
            playlist.complete = true;
        } else if !line.starts_with('#') {
            let duration = pending.take().context("HLS segment duration is missing")?;
            ensure!(line == segment_name(playlist.segments.len()) && media_filename(line), "invalid HLS segment URI");
            playlist.duration += duration;
            playlist.durations.push(duration);
            playlist.segments.push(line.to_string());
        } else {
            ensure!(matches!(line, "#EXTM3U" | "#EXT-X-INDEPENDENT-SEGMENTS"
                | "#EXT-X-MEDIA-SEQUENCE:0" | "#EXT-X-PLAYLIST-TYPE:EVENT" | "#EXT-X-MAP:URI=\"init.mp4\"")
                || line.strip_prefix("#EXT-X-TARGETDURATION:").is_some_and(|value| value.parse::<u32>().is_ok())
                || line.strip_prefix("#EXT-X-VERSION:").is_some_and(|value| value.parse::<u32>().is_ok()),
                "unsupported HLS tag: {line}");
        }
    }
    ensure!(pending.is_none() && playlist.duration.is_finite(), "incomplete HLS playlist");
    Ok(playlist)
}

/// 按连续序号生成正式分片文件名.
pub(crate) fn segment_name(index: usize) -> String { format!("segment_{index:06}.m4s") }

/// 限制 HTTP 可读取的播放资源名称.
pub(crate) fn media_filename(name: &str) -> bool {
    name == "index.m3u8" || name == "init.mp4"
        || name.strip_prefix("segment_").and_then(|name| name.strip_suffix(".m4s"))
            .is_some_and(|number| number.len() == 6 && number.bytes().all(|byte| byte.is_ascii_digit()))
}

/// 只保存已验证的连续前缀, 缓存末端来自样本时间轴, 而不是源视频的估算时长.
#[derive(Clone, Default)]
pub(crate) struct CachedMedia {
    /// 已校验的初始化片段.
    pub(crate) init: Option<Init>,
    /// 已校验的连续媒体分片.
    pub(crate) segments: Vec<Fragment>,
    /// 是否已经确认媒体结束并提交 ENDLIST.
    pub(crate) complete: bool,
}

impl CachedMedia {
    /// 返回最后一个已发布样本的精确时间轴终点.
    pub(crate) fn tail(&self) -> Tail { self.segments.last().map(|segment| segment.end).unwrap_or_default() }

    /// 根据样本时间轴生成固定格式的 HLS 清单.
    pub(crate) fn playlist(&self) -> Result<String> {
        let mut text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:0\n\
            #EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init.mp4\"\n".to_string();
        for (index, segment) in self.segments.iter().enumerate() {
            let duration = (segment.end.video - segment.start.video) as f64 / 90_000.0;
            ensure!(duration > 0.0 && duration.round() <= 5.0, "video segment exceeds the fixed HLS target duration");
            text.push_str(&format!("#EXTINF:{duration:.9},\n{}\n", segment_name(index)));
        }
        if self.complete { text.push_str("#EXT-X-ENDLIST\n"); }
        Ok(text)
    }
}

/// 限制媒体文件类型和读取大小, 拒绝符号链接与空文件.
pub(crate) async fn regular_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    ensure!(metadata.is_file() && metadata.len() > 0 && metadata.len() <= limit,
        "unsafe, empty or oversized HLS file: {}", path.display());
    Ok(tokio::fs::read(path).await?)
}

/// 判断错误是否仅表示尚未生成文件.
fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// 幂等删除已知的单个媒体文件, 其他 I/O 错误必须上报.
pub(super) async fn remove_file(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 仅在已有可播放分片时发布正式清单.
pub(crate) async fn write_playlist(directory: &Path, media: &CachedMedia) -> Result<()> {
    if !media.segments.is_empty() {
        atomic_file(directory, "index.m3u8", media.playlist()?.as_bytes()).await?;
    }
    Ok(())
}

/// 首片额外探测编码参数, 每片都与指定 init 拼接后真实解码全部音视频样本.
impl ProcessManager {
/// 在统一进程预算内验证输出参数并解码整个分片.
pub(crate) async fn validate_segment(&self, directory: &Path, init: &str, segment: &str, audio: bool,
    probe: bool, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let _permit = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(Cancelled.into()),
        permit = self.process_slots.acquire() => permit?,
    };
    let input = format!("concat:{init}|{segment}");
    if probe {
        let mut command = Command::new("ffprobe");
        command.current_dir(directory).args(["-v", "error", "-show_entries",
            "stream=codec_type,codec_name,pix_fmt,profile,channels,sample_rate", "-of", "json", &input]);
        let result = checked_output_cancellable(&mut command, 15, cancel).await?;
        /// 只读取验证固定编码契约所需的流信息.
        #[derive(Deserialize)]
        struct Output {
            /// 编码后输出的媒体流.
            streams: Vec<Stream>,
        }
        /// 描述待验证分片的单条输出流.
        #[derive(Deserialize)]
        struct Stream {
            /// 媒体流类型.
            codec_type: String,
            /// 输出编码器名称.
            codec_name: String,
            /// 视频像素格式.
            pix_fmt: Option<String>,
            /// 视频或音频编码配置.
            profile: Option<String>,
            /// 音频声道数.
            channels: Option<u32>,
            /// 音频采样率.
            sample_rate: Option<String>,
        }
        let output: Output = serde_json::from_slice(&result)?;
        ensure!(output.streams.len() == if audio { 2 } else { 1 }, "unexpected HLS stream count");
        let video = output.streams.iter().find(|stream| stream.codec_type == "video").context("output has no video")?;
        ensure!(video.codec_name == "h264" && video.pix_fmt.as_deref() == Some("yuv420p")
            && video.profile.as_deref() == Some("High"), "output is not 8-bit H.264 High");
        if audio {
            let audio = output.streams.iter().find(|stream| stream.codec_type == "audio").context("output has no audio")?;
            ensure!(audio.codec_name == "aac" && audio.profile.as_deref() == Some("LC")
                && audio.channels == Some(2) && audio.sample_rate.as_deref() == Some("48000"),
                "output is not stereo AAC-LC at 48 kHz");
        }
    }
    // ffprobe 只能证明参数可读, 必须实际解码全部样本后才允许发布.
    let mut command = Command::new("ffmpeg");
    command.current_dir(directory).args(["-nostdin", "-v", "error", "-xerror", "-err_detect", "explode",
        "-threads", "1", "-filter_threads", "1", "-i", &input, "-map", "0:v", "-map", "0:a?",
        "-f", "null", "-"]);
    checked_output_cancellable(&mut command, 15, cancel).await?;
    Ok(())
}

/// 从 0 恢复最长有效前缀, 修复清单并删除损坏尾部, 不在恢复期间启动续接编码.
pub(crate) async fn recover(&self, directory: &Path, cancel: &mut watch::Receiver<bool>) -> Result<CachedMedia> {
    // 读取磁盘的 I/O 错误直接上报, 不能因权限等问题把已有缓存当作空目录清理.
    let original = match read_playlist(directory).await {
        Ok(playlist) => playlist,
        Err(error) if error.downcast_ref::<std::io::Error>().is_some() => return Err(error),
        Err(error) => { warn!(path = %directory.display(), "repairing invalid cached playlist: {error:#}"); None },
    };
    let mut media = CachedMedia::default();
    match regular_bytes(&directory.join("init.mp4"), 4 * 1024 * 1024).await {
        Ok(bytes) => match Init::parse(&bytes) {
            Ok(init) => media.init = Some(init),
            Err(error) => warn!(path = %directory.display(), "discarding incompatible or corrupt cache init: {error:#}"),
        },
        Err(error) if missing(&error) => {},
        Err(error) if error.downcast_ref::<std::io::Error>().is_some() => return Err(error),
        Err(error) => warn!(path = %directory.display(), "discarding invalid cache init: {error:#}"),
    }
    if let Some(init) = &media.init {
        for index in 0..1_000_000 {
            check_cancel(cancel)?;
            let path = directory.join(segment_name(index));
            let bytes = match regular_bytes(&path, 256 * 1024 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) if missing(&error) => break,
                Err(error) if error.downcast_ref::<std::io::Error>().is_some() => return Err(error),
                Err(error) => { warn!(path = %path.display(), "discarding invalid cache tail: {error:#}"); break; },
            };
            match fragment::inspect(&bytes, init, media.tail(), index as u32 + 1) {
                Ok(segment) => media.segments.push(segment),
                Err(error) => { warn!(path = %path.display(), "discarding discontinuous cache tail: {error:#}"); break; },
            }
        }
        // 中间分片逐一校验结构, 首段和最后保留段再执行真实解码以确认前缀可用.
        if !media.segments.is_empty() {
            if let Err(error) = self.validate_segment(directory, "init.mp4", &segment_name(0), init.audio(), true, cancel).await {
                if *cancel.borrow() { return Err(error); }
                warn!(path = %directory.display(), "discarding undecodable cache prefix: {error:#}");
                media.segments.clear();
            }
            while media.segments.len() > 1 {
                let name = segment_name(media.segments.len() - 1);
                match self.validate_segment(directory, "init.mp4", &name, init.audio(), false, cancel).await {
                    Ok(()) => break,
                    Err(error) => {
                        if *cancel.borrow() { return Err(error); }
                        warn!(path = %directory.display(), "discarding undecodable cache tail: {error:#}");
                        media.segments.pop();
                    },
                }
            }
        }
    }
    // ENDLIST 在实际 EOF 且所有分片验证通过之后发布, 可恢复标记落盘前的中断.
    media.complete = original.as_ref().is_some_and(|playlist| playlist.complete
        && playlist.segments.len() == media.segments.len() && !media.segments.is_empty()
        && playlist.durations.iter().zip(&media.segments).all(|(duration, segment)|
            (duration - (segment.end.video - segment.start.video) as f64 / 90_000.0).abs() < 0.00001));
    let mut files = tokio::fs::read_dir(directory).await?;
    while let Some(file) = files.next_entry().await? {
        let name = file.file_name().to_string_lossy().into_owned();
        let index = name.strip_prefix("segment_").and_then(|name| name.strip_suffix(".m4s"))
            .filter(|index| index.len() == 6 && index.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|index| index.parse::<usize>().ok());
        if name.strip_prefix(".work-").is_some_and(crate::utils::check_md5_ok) {
            if file.file_type().await?.is_dir() { tokio::fs::remove_dir_all(file.path()).await?; }
            else { tokio::fs::remove_file(file.path()).await?; }
        } else if index.is_some_and(|index| index >= media.segments.len())
            || matches!(name.as_str(), "init.mp4.tmp" | "index.m3u8.tmp" | "complete.tmp")
            || (name == "complete" && !media.complete) {
            tokio::fs::remove_file(file.path()).await?;
        }
    }
    if media.segments.is_empty() {
        media.init = None;
        remove_file(&directory.join("init.mp4")).await?;
        remove_file(&directory.join("index.m3u8")).await?;
    } else {
        write_playlist(directory, &media).await?;
        if media.complete { atomic_file(directory, "complete", b"").await?; }
    }
    Ok(media)
}
}

/// 以 90 kHz 整数刻度续接, 不重设全片采样相位.
fn video_timing(metadata: &MediaMetadata, resume: Tail) -> String {
    format!("settb=expr=1/90000,setpts=PTS-{},\
        fps=fps='min(source_fps,60)':start_time=0,settb=expr=1/90000,\
        trim=start_pts={},setpts=PTS-{}",
        (metadata.start_time * 90_000.0).round() as i64, resume.video, resume.video)
}
