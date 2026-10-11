//! 构建顺序转码命令并恢复和验证持久 HLS.

use std::{path::{Path, PathBuf}, process::Stdio};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Deserializer};
use tokio::{io::AsyncWriteExt, process::Command, sync::watch};
use tracing::warn;

use super::{assets::RenderAssets, cache::{is_key, PlaybackVariantConfig, ENCODER_PROFILE},
    fragment::{self, Fragment, Init, Tail}, metadata::{MediaInfo, Track},
    process::{cancelled, check_cancel, checked_output_cancellable, Cancelled, Processes}};

/// 客户端只表达选轨和位置意图, 完整播放配置由服务端构造.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackRequest {

    // 缺省表示服务端选轨, 显式 null 表示关闭, 两者不能混为一谈.
    #[serde(default, deserialize_with = "track_choice")]
    pub audio_stream_index: Option<Option<i32>>,
    #[serde(default, deserialize_with = "track_choice")]
    pub subtitle_stream_index: Option<Option<i32>>,
    pub position: Option<f64>,
}

impl PlaybackRequest {
    /// 仅将缺省字段补为数据库偏好, 显式 null 仍表示本次关闭对应轨道.
    pub fn with_preferences(mut self, audio: Option<i32>, subtitle: Option<i32>) -> Self {
        if self.audio_stream_index.is_none() { self.audio_stream_index = Some(audio); }
        if self.subtitle_stream_index.is_none() { self.subtitle_stream_index = Some(subtitle); }
        self
    }
}

fn track_choice<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(deserializer).map(Some)
}

/// 单次任务的执行参数, 只有 config 参与缓存身份计算, 其余字段用于媒体处理和调度.
#[derive(Clone)]
pub struct EncodeSpec {
    pub config: PlaybackVariantConfig,
    pub source: PathBuf,
    pub duration: f64,
    // 容器时间轴原点可能为负数, 视频, 音频和字幕都要按同一原点归零.
    pub source_start: f64,
    pub info: MediaInfo,
    pub subtitle: Option<Track>,
    pub hdr: bool,
    pub target: f64,
}

impl EncodeSpec {
    pub fn new(id: &str, source: PathBuf, info: MediaInfo, request: PlaybackRequest,
        saved_position: Option<f64>) -> Result<Self> {
        ensure!(is_key(id), "invalid source file ID");
        let saved = saved_position.unwrap_or(0.0);
        ensure!(saved.is_finite() && saved >= 0.0, "invalid saved playback position");
        // 保留原有体验: 已到结尾的观看记录从头播放, 显式换轨位置则严格校验.
        let target = request.position.unwrap_or(if saved < info.duration - 2.0 { saved } else { 0.0 });
        ensure!(target.is_finite() && target >= 0.0 && target < info.duration, "position must be within the video duration");
        let video = info.video_tracks.first().context("media has no playable video stream")?;
        let audio_index = request.audio_stream_index.unwrap_or_else(|| {
            info.audio_tracks.iter().find(|track| track.is_default).or(info.audio_tracks.first()).map(|track| track.stream_index)
        });
        let subtitle_index = request.subtitle_stream_index.unwrap_or_else(|| {
            info.subtitle_tracks.iter().find(|track| track.is_default || track.is_forced).map(|track| track.stream_index)
        });
        let audio = select(&info.audio_tracks, audio_index, "audio")?;
        let subtitle = select(&info.subtitle_tracks, subtitle_index, "subtitle")?;
        let config = PlaybackVariantConfig {
            src_file_pre16mib_md5: id.to_string(), video_stream: video.stream_index,
            audio_stream: audio.map(|track| track.stream_index),
            subtitle_stream: subtitle.as_ref().map(|track| track.stream_index),
            encoder_profile: ENCODER_PROFILE.to_string(),
        };
        Ok(Self {
            config, source, duration: info.duration, source_start: info.start_time,
            hdr: video.is_hdr(), info, subtitle, target,
        })
    }
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}

/// 只向私有工作目录生成 HLS, 正式缓存由 CacheManager 校验后原子追加.
pub fn command(spec: &EncodeSpec, directory: &Path, assets: Option<&RenderAssets>,
    resume: Tail, threads: usize) -> Result<Command> {
    let mut command = Command::new("ffmpeg");
    command.current_dir(directory).kill_on_drop(true)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.args(["-nostdin", "-hide_banner", "-v", "warning", "-xerror", "-y", "-copyts",
        "-threads", &threads.to_string(), "-filter_threads", "1", "-filter_complex_threads", "1"]);
    let seek = (resume.seconds() - 1.0).max(0.0);
    if seek > 0.0 { command.args(["-ss", &format!("{seek:.9}")]); }
    command.arg("-i").arg(&spec.source);
    let bitmap = spec.subtitle.as_ref().filter(|track| track.is_bitmap());
    let history = usize::from(resume.video > 0 && (spec.config.audio_stream.is_some() || bitmap.is_some()));
    if history == 1 {
        // 音频重采样和位图字幕依赖历史状态. 第二路只解码这些流, 不重编码已有视频.
        command.args(["-threads", "1", "-i"]).arg(&spec.source);
    }
    let mut filters = Vec::new();
    if spec.hdr {
        filters.push("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv".to_string());
    }
    filters.push("scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2".to_string());
    filters.push("format=yuv420p".to_string());
    if spec.subtitle.as_ref().is_some_and(|track| !track.is_bitmap()) {
        filters.push(assets.context("text subtitle resources are missing")?.filter());
    }
    // 先按原时间烧录字幕, 再固定有理数帧率和全片采样原点, 避免续接重置选帧相位.
    filters.push(video_timing(spec, resume));
    if let Some(track) = bitmap {
        command.args(["-filter_complex",
            &format!("[0:{}][{history}:{}]overlay=eof_action=pass,{}[video]",
                spec.config.video_stream, track.stream_index, filters.join(",")),
            "-map", "[video]"]);
    } else {
        command.args(["-map", &format!("0:{}", spec.config.video_stream), "-vf", &filters.join(",")]);
    }
    // 禁用 B 帧以统一显示和解码时间, 定期关键帧保证每个分片可独立解码.
    command.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
        "-pix_fmt", "yuv420p", "-profile:v", "high", "-threads", &threads.to_string(),
        "-bf", "0", "-force_key_frames", "expr:gte(t,n_forced*4)", "-sc_threshold", "0",
        "-fps_mode", "passthrough", "-enc_time_base:v", "1:90000"]);
    if spec.hdr { command.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]); }
    if let Some(index) = spec.config.audio_stream {
        let warmup = resume.audio.min(8192);
        let samples = (spec.duration * 48_000.0).ceil() as u64;
        // 从固定采样原点裁切, 丢弃 AAC 编码延迟和预热包, 而不是把它们追加到旧音轨.
        let audio = format!("asetpts=PTS-{:.9}/TB,aresample=48000:async=1:first_pts=0,\
            apad=whole_len={samples},atrim=end_sample={samples},atrim=start_sample={},asetpts=(N-{warmup})/SR/TB",
            spec.source_start, resume.audio - warmup);
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

// 续接点以 90 kHz 整数刻度裁切, 不反复累加浮点秒数或重设全片采样原点.
fn video_timing(spec: &EncodeSpec, resume: Tail) -> String {
    format!("settb=expr=1/90000,setpts=PTS-{},\
        fps=fps='min(source_fps,60)':start_time=0,settb=expr=1/90000,\
        trim=start_pts={},setpts=PTS-{}",
        (spec.source_start * 90_000.0).round() as i64, resume.video, resume.video)
}

pub async fn has_video_after(spec: &EncodeSpec, resume: Tail, processes: &Processes,
    cancel: &mut watch::Receiver<bool>) -> Result<bool> {
    let _permit = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(Cancelled.into()),
        permit = processes.slots.acquire() => permit?,
    };
    // 最后一个分片提交后也可能恰好中断. 用真实解码确认 EOF, 不靠时长容差猜测完成.
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-v", "error", "-xerror", "-copyts", "-threads", "1",
        "-filter_threads", "1", "-ss", &format!("{:.9}", (resume.seconds() - 1.0).max(0.0)), "-i"])
        .arg(&spec.source).args(["-map", &format!("0:{}", spec.config.video_stream),
            "-vf", &video_timing(spec, resume), "-frames:v", "1", "-threads:v", "1",
            "-an", "-sn", "-dn", "-f", "framemd5", "-"]);
    let output = checked_output_cancellable(&mut command, 60, cancel).await?;
    let text = std::str::from_utf8(&output)?;
    ensure!(text.starts_with("#format: frame checksums"), "invalid resume frame check output");
    Ok(text.lines().any(|line| !line.is_empty() && !line.starts_with('#')))
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub segments: Vec<String>,
    pub durations: Vec<f64>,
    pub duration: f64,
    pub complete: bool,
}

pub async fn read_playlist(directory: &Path) -> Result<Option<Playlist>> {
    let path = directory.join("index.m3u8");
    let content = match regular_bytes(&path, 16 * 1024 * 1024).await {
        Ok(content) => content,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(parse_playlist(std::str::from_utf8(&content)?)?))
}

// 仅接受本项目从 0 连续编号的本地资源, 不把外部 URI 或断片清单当作有效缓存.
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

pub fn segment_name(index: usize) -> String { format!("segment_{index:06}.m4s") }

pub fn media_filename(name: &str) -> bool {
    name == "index.m3u8" || name == "init.mp4"
        || name.strip_prefix("segment_").and_then(|name| name.strip_suffix(".m4s"))
            .is_some_and(|number| number.len() == 6 && number.bytes().all(|byte| byte.is_ascii_digit()))
}

/// 只保存已验证的连续前缀, 缓存末端来自样本时间轴, 而不是源视频的估算时长.
#[derive(Clone, Default)]
pub struct CachedMedia {
    pub init: Option<Init>,
    pub segments: Vec<Fragment>,
    pub complete: bool,
}

impl CachedMedia {
    pub fn tail(&self) -> Tail { self.segments.last().map(|segment| segment.end).unwrap_or_default() }

    pub fn playlist(&self) -> Result<String> {
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

pub async fn regular_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    ensure!(metadata.is_file() && metadata.len() > 0 && metadata.len() <= limit,
        "unsafe, empty or oversized HLS file: {}", path.display());
    Ok(tokio::fs::read(path).await?)
}

fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

pub async fn remove_file(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub async fn atomic_file(directory: &Path, name: &str, content: &[u8]) -> Result<()> {
    // 文件数据和目录项按顺序落盘, 重启只会看到旧版本或完整的新版本.
    let temporary = directory.join(format!("{name}.tmp"));
    let mut file = tokio::fs::File::create(&temporary).await?;
    file.write_all(content).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temporary, directory.join(name)).await?;
    sync_directory(directory).await
}

pub async fn sync_directory(directory: &Path) -> Result<()> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(directory)?.sync_all()).await??;
    Ok(())
}

pub async fn write_playlist(directory: &Path, media: &CachedMedia) -> Result<()> {
    if !media.segments.is_empty() {
        atomic_file(directory, "index.m3u8", media.playlist()?.as_bytes()).await?;
    }
    Ok(())
}

/// 首片额外探测编码参数, 每片都与指定 init 拼接后真实解码全部音视频样本.
pub async fn validate_segment(directory: &Path, init: &str, segment: &str, audio: bool,
    probe: bool, processes: &Processes, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let _permit = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(Cancelled.into()),
        permit = processes.slots.acquire() => permit?,
    };
    let input = format!("concat:{init}|{segment}");
    if probe {
        let mut command = Command::new("ffprobe");
        command.current_dir(directory).args(["-v", "error", "-show_entries",
            "stream=codec_type,codec_name,pix_fmt,profile,channels,sample_rate", "-of", "json", &input]);
        let result = checked_output_cancellable(&mut command, 15, cancel).await?;
        #[derive(Deserialize)]
        struct Output { streams: Vec<Stream> }
        #[derive(Deserialize)]
        struct Stream {
            codec_type: String, codec_name: String, pix_fmt: Option<String>, profile: Option<String>,
            channels: Option<u32>, sample_rate: Option<String>,
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
pub async fn recover(directory: &Path, processes: &Processes, cancel: &mut watch::Receiver<bool>) -> Result<CachedMedia> {
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
            if let Err(error) = validate_segment(directory, "init.mp4", &segment_name(0), init.audio(), true, processes, cancel).await {
                if *cancel.borrow() { return Err(error); }
                warn!(path = %directory.display(), "discarding undecodable cache prefix: {error:#}");
                media.segments.clear();
            }
            while media.segments.len() > 1 {
                let name = segment_name(media.segments.len() - 1);
                match validate_segment(directory, "init.mp4", &name, init.audio(), false, processes, cancel).await {
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
        if name.strip_prefix(".work-").is_some_and(is_key) {
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


// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

#[cfg(test)]
pub fn fixture() -> (MediaInfo, PlaybackRequest) {
    let info = serde_json::from_value(serde_json::json!({
        "duration": 20.0, "start_time": 0.0,
        "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
        "audio_tracks": [{"stream_index": 1, "codec_name": "aac", "is_default": true, "is_forced": false}],
        "subtitle_tracks": [{"stream_index": 2, "codec_name": "ass", "is_default": false, "is_forced": false}],
        "attachments": []
    })).unwrap();
    (info, PlaybackRequest::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_explicit_off_and_saved_positions_are_distinct() {
        // file ID 默认请求使用服务端进度和默认轨道, null 仍可关闭音轨.
        let (info, request) = fixture();
        let spec = EncodeSpec::new(&"a".repeat(32), "video".into(), info.clone(), request, Some(7.375)).unwrap();
        assert_eq!(spec.target, 7.375);
        assert_eq!(spec.config.audio_stream, Some(1));
        let request = serde_json::from_str(r#"{"audio_stream_index":null,"subtitle_stream_index":2,"position":8.5}"#).unwrap();
        let changed = EncodeSpec::new(&"a".repeat(32), "video".into(), info.clone(), request, Some(3.0)).unwrap();
        assert_eq!(changed.target, 8.5);
        assert_eq!(changed.config.audio_stream, None);
        assert_eq!(changed.config.subtitle_stream, Some(2));
        assert_eq!(EncodeSpec::new(&"a".repeat(32), "video".into(), info, PlaybackRequest::default(), Some(19.0)).unwrap().target, 0.0);
    }

    #[test]
    fn saved_tracks_fill_only_omitted_fields() {
        let omitted: PlaybackRequest = serde_json::from_str("{}").unwrap();
        let resolved = omitted.with_preferences(Some(4), Some(7));
        assert_eq!(resolved.audio_stream_index, Some(Some(4)));
        assert_eq!(resolved.subtitle_stream_index, Some(Some(7)));

        let (info, _) = fixture();
        let chosen = PlaybackRequest::default().with_preferences(Some(1), Some(2));
        let spec = EncodeSpec::new(&"a".repeat(32), "video".into(), info.clone(), chosen, None).unwrap();
        assert_eq!(spec.config.audio_stream, Some(1));
        assert_eq!(spec.config.subtitle_stream, Some(2));

        let explicit: PlaybackRequest = serde_json::from_str(r#"{"audio_stream_index":null}"#).unwrap();
        let resolved = explicit.with_preferences(Some(4), Some(2));
        assert_eq!(resolved.audio_stream_index, Some(None));
        assert_eq!(resolved.subtitle_stream_index, Some(Some(2)));
        let spec = EncodeSpec::new(&"a".repeat(32), "video".into(), info, resolved, None).unwrap();
        assert_eq!(spec.config.audio_stream, None);
        assert_eq!(spec.config.subtitle_stream, Some(2));
    }

    #[test]
    fn invalid_tracks_positions_and_client_profiles_fail() {
        for position in [-1.0, f64::NAN, f64::INFINITY, 20.0] {
            let (info, mut request) = fixture();
            request.position = Some(position);
            assert!(EncodeSpec::new(&"a".repeat(32), "video".into(), info, request, None).is_err());
        }
        for value in [
            serde_json::json!({"video_stream_index":0}), serde_json::json!({"encoder_profile":ENCODER_PROFILE}),
            serde_json::json!({"variant_key":"a".repeat(32)}), serde_json::json!({"source_version":"old"}),
        ] {
            assert!(serde_json::from_value::<PlaybackRequest>(value).is_err());
        }
        for field in ["audio_stream_index", "subtitle_stream_index"] {
            let (info, _) = fixture();
            let request = serde_json::from_value(serde_json::json!({field:99})).unwrap();
            assert!(EncodeSpec::new(&"a".repeat(32), "video".into(), info, request, None).is_err());
        }
    }

    #[test]
    fn positions_threads_and_paths_do_not_enter_the_configuration() {
        let (info, request) = fixture();
        let spec = EncodeSpec::new(&"a".repeat(32), "video.mp4".into(), info.clone(), request, Some(7.375)).unwrap();
        let zero = EncodeSpec::new(&"a".repeat(32), "other.mp4".into(), info, PlaybackRequest::default(), None).unwrap();
        assert_eq!(spec.config, zero.config);
        let command = command(&spec, Path::new("/tmp"), None, Tail::default(), 8).unwrap();
        let args: Vec<_> = command.as_std().get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert!(!args.contains(&"-ss"));
        for pair in [["-c:v", "libx264"], ["-bf", "0"], ["-threads", "8"],
            ["-fps_mode", "passthrough"], ["-enc_time_base:v", "1:90000"]] {
            assert!(args.windows(2).any(|args| args == pair));
        }
        let command = super::command(&spec, Path::new("/tmp"), None, Tail { video: 720000, audio: 385024 }, 2).unwrap();
        let args: Vec<_> = command.as_std().get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|args| args == ["-ss", "7.000000000"]));
        assert!(args.iter().any(|arg| arg.contains("atrim=start_sample=376832")));
        assert_eq!(args.iter().filter(|arg| **arg == "-i").count(), 2);
    }

    #[test]
    fn playlists_require_local_continuous_segments_and_finite_durations() {
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nsegment_000000.m4s\n#EXT-X-ENDLIST\n";
        let playlist = parse_playlist(text).unwrap();
        assert!(playlist.complete);
        assert_eq!(playlist.duration, 4.0);
        for invalid in [
            text.replace("#EXTINF:4,", "#EXTINF:NaN,"),
            text.replace("segment_000000.m4s", "../init.mp4"),
            text.replace("segment_000000.m4s", "segment_000001.m4s"),
            text.replace("segment_000000.m4s\n", ""),
            format!("{text}#EXTINF:4,\nsegment_000001.m4s\n"),
        ] { assert!(parse_playlist(&invalid).is_err()); }
        for name in ["../main.db", "complete", ".work-abc", "subtitle.ass", "segment_1.m4s", "index.m3u8.tmp"] {
            assert!(!media_filename(name));
        }
    }
}
