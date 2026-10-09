use std::{path::{Path, PathBuf}, process::Stdio};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::{metadata::{Attachment, MediaInfo, Track}, process::checked_output};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackRequest {
    pub source_version: String,
    pub video_stream_index: Option<i32>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    #[serde(default)]
    pub position: f64,
}

pub struct EncodeSpec {
    pub source: PathBuf,
    pub start: f64,
    pub duration: f64,
    pub source_start: f64,
    pub video_index: i32,
    pub audio_index: Option<i32>,
    pub subtitle: Option<Track>,
    pub attachments: Vec<Attachment>,
    pub hdr: bool,
    pub threads: usize,
}

impl EncodeSpec {
    pub fn new(source: PathBuf, info: MediaInfo, request: PlaybackRequest, threads: usize) -> Result<Self> {
        ensure!(request.source_version == info.source_version, "source changed; reload media information");
        ensure!(request.position.is_finite() && request.position >= 0.0 && request.position < info.duration,
            "position must be within the video duration");
        let video = match request.video_stream_index {
            Some(index) => info.video_tracks.iter().find(|track| track.stream_index == index),
            None => info.video_tracks.first(),
        }.context("selected video track does not exist")?;
        let audio = select(&info.audio_tracks, request.audio_stream_index, "audio")?;
        let subtitle = select(&info.subtitle_tracks, request.subtitle_stream_index, "subtitle")?;
        Ok(Self {
            source, start: request.position, duration: info.duration, source_start: info.start_time,
            video_index: video.stream_index, audio_index: audio.map(|track| track.stream_index),
            hdr: video.is_hdr(), subtitle, attachments: info.attachments, threads,
        })
    }
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}

pub fn command(spec: &EncodeSpec, directory: &Path) -> Command {
    let mut command = Command::new("ffmpeg");
    command.current_dir(directory).kill_on_drop(true)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.args(["-nostdin", "-hide_banner", "-v", "warning", "-y",
        "-threads", &spec.threads.to_string(), "-filter_threads", "1", "-filter_complex_threads", "1"]);
    if spec.start > 0.0 { command.args(["-ss", &format!("{:.6}", spec.start)]); }
    command.arg("-i").arg(&spec.source);
    let bitmap = spec.subtitle.as_ref().filter(|track| track.is_bitmap());
    if bitmap.is_some() {
        // A bitmap cue can start before the seek target and remain visible after it.
        command.args(["-threads", "1", "-i"]).arg(&spec.source);
    }
    let mut filters = Vec::new();
    if spec.hdr {
        filters.push("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv".to_string());
    }
    filters.push("scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2".to_string());
    filters.push("format=yuv420p".to_string());
    if let Some(subtitle) = spec.subtitle.as_ref().filter(|track| !track.is_bitmap()) {
        // Embedded subtitles retain source timestamps; sidecars use the media-relative timeline.
        let offset = spec.start + if subtitle.external_path.is_none() { spec.source_start } else { 0.0 };
        filters.push(format!("setpts=PTS+{offset:.6}/TB,subtitles=filename='subtitle.ass':fontsdir='fonts',setpts=PTS-STARTPTS"));
    }
    if let Some(track) = bitmap {
        command.args(["-filter_complex",
            &format!("[1:{}]setpts=PTS-{:.6}/TB[subtitle];[0:{}][subtitle]overlay=eof_action=pass,{}[video]",
                track.stream_index, spec.start, spec.video_index, filters.join(",")),
            "-map", "[video]"]);
    } else {
        command.args(["-map", &format!("0:{}", spec.video_index), "-vf", &filters.join(",")]);
    }
    command.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
        "-pix_fmt", "yuv420p", "-profile:v", "high", "-threads", &spec.threads.to_string(),
        "-force_key_frames", "expr:gte(t,n_forced*4)", "-sc_threshold", "0", "-fpsmax", "60"]);
    if spec.hdr { command.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]); }
    if let Some(index) = spec.audio_index {
        command.args(["-map", &format!("0:{index}"), "-c:a", "aac", "-profile:a", "aac_low",
            "-ac", "2", "-ar", "48000", "-b:a", "192k"]);
    } else { command.arg("-an"); }
    command.args(["-sn", "-dn", "-map_metadata", "-1", "-avoid_negative_ts", "make_zero",
        "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", "4",
        "-hls_list_size", "0", "-hls_playlist_type", "event",
        "-hls_flags", "independent_segments+temp_file", "-hls_fmp4_init_filename", "init.mp4",
        "-hls_segment_filename", "segment_%06d.m4s", "index.m3u8"]);
    command
}

pub async fn playlist_duration(directory: &Path) -> Result<Option<f64>> {
    let content = match tokio::fs::read_to_string(directory.join("index.m3u8")).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut duration = 0.0;
    for line in content.lines().filter_map(|line| line.strip_prefix("#EXTINF:")) {
        let value = line.split(',').next().context("invalid HLS duration")?.parse::<f64>()?;
        ensure!(value.is_finite() && value > 0.0, "invalid HLS segment duration");
        duration += value;
    }
    Ok((duration > 0.0).then_some(duration))
}

pub fn media_filename(name: &str) -> bool {
    name == "index.m3u8" || name == "init.mp4"
        || name.strip_prefix("segment_").and_then(|s| s.strip_suffix(".m4s"))
            .is_some_and(|s| s.len() == 6 && s.bytes().all(|c| c.is_ascii_digit()))
}

pub async fn validate_first_segment(directory: &Path, audio: bool) -> Result<()> {
    let content = tokio::fs::read_to_string(directory.join("index.m3u8")).await?;
    ensure!(content.starts_with("#EXTM3U") && content.contains("#EXT-X-MAP:URI=\"init.mp4\""),
        "invalid fMP4 playlist");
    let first = content.lines().find(|line| !line.starts_with('#') && !line.is_empty())
        .context("playlist contains no segments")?;
    ensure!(media_filename(first) && first.ends_with(".m4s"), "unsafe HLS segment path");
    let mut bytes = tokio::fs::read(directory.join("init.mp4")).await?;
    ensure!(!bytes.is_empty(), "empty HLS initialization segment");
    let segment = tokio::fs::read(directory.join(first)).await?;
    ensure!(!segment.is_empty(), "empty HLS media segment");
    bytes.extend(segment);
    let probe_path = directory.join("probe.mp4");
    tokio::fs::write(&probe_path, bytes).await?;
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-show_entries",
        "stream=codec_type,codec_name,pix_fmt,profile,channels,sample_rate", "-of", "json"]).arg(&probe_path);
    let result = checked_output(&mut command, 15).await;
    tokio::fs::remove_file(&probe_path).await?;
    #[derive(Deserialize)]
    struct Output { streams: Vec<Stream> }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: String, codec_name: String, pix_fmt: Option<String>, profile: Option<String>,
        channels: Option<u32>, sample_rate: Option<String>,
    }
    let output: Output = serde_json::from_slice(&result?)?;
    ensure!(output.streams.len() == if audio { 2 } else { 1 }, "unexpected HLS stream count");
    let video = output.streams.iter().find(|s| s.codec_type == "video").context("output has no video")?;
    ensure!(video.codec_name == "h264" && video.pix_fmt.as_deref() == Some("yuv420p")
        && video.profile.as_deref() == Some("High"), "output is not 8-bit H.264 High");
    if audio {
        let audio = output.streams.iter().find(|s| s.codec_type == "audio").context("output has no audio")?;
        ensure!(audio.codec_name == "aac" && audio.profile.as_deref() == Some("LC")
            && audio.channels == Some(2) && audio.sample_rate.as_deref() == Some("48000"),
            "output is not stereo AAC-LC at 48 kHz");
    }
    Ok(())
}

#[cfg(test)]
pub fn fixture() -> (MediaInfo, PlaybackRequest) {
    let info = serde_json::from_value(serde_json::json!({
        "source_version": "test", "duration": 20.0, "start_time": 0.0,
        "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
        "audio_tracks": [{"stream_index": 1, "codec_name": "aac", "is_default": true, "is_forced": false}],
        "subtitle_tracks": [{"stream_index": 2, "codec_name": "ass", "is_default": false, "is_forced": false}],
        "attachments": []
    })).unwrap();
    let request = PlaybackRequest {
        source_version: "test".to_string(), video_stream_index: None,
        audio_stream_index: Some(1), subtitle_stream_index: None, position: 0.0,
    };
    (info, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_tracks_versions_and_positions_fail() {
        for position in [-1.0, f64::NAN, f64::INFINITY, 20.0] {
            let (info, mut request) = fixture();
            request.position = position;
            assert!(EncodeSpec::new("video".into(), info, request, 1).is_err());
        }
        for field in ["video_stream_index", "audio_stream_index", "subtitle_stream_index", "source_version"] {
            let (info, _) = fixture();
            let mut value = serde_json::json!({"source_version": "test"});
            value[field] = if field == "source_version" { "old".into() } else { 99.into() };
            let request = serde_json::from_value(value).unwrap();
            assert!(EncodeSpec::new("video".into(), info, request, 1).is_err());
        }
        assert!(serde_json::from_value::<PlaybackRequest>(
            serde_json::json!({"source_version": "test", "force_transcode": true})).is_err());
    }

    #[test]
    fn all_sources_use_one_profile_and_fractional_start() {
        let (info, mut request) = fixture();
        request.position = 7.375;
        let spec = EncodeSpec::new("video.mp4".into(), info, request, 2).unwrap();
        assert_eq!(spec.start, 7.375);
        let command = command(&spec, Path::new("/tmp"));
        let args: Vec<_> = command.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        for pair in [["-c:v", "libx264"], ["-pix_fmt", "yuv420p"], ["-profile:v", "high"],
            ["-c:a", "aac"], ["-hls_segment_type", "fmp4"], ["-ss", "7.375000"]] {
            assert!(args.windows(2).any(|args| args == pair));
        }
        assert!(!args.contains(&"copy"));
        assert!(!args.iter().any(|arg| arg.contains("subtitles=")));
    }

    #[test]
    fn text_and_bitmap_subtitles_are_burned() {
        for codec in ["ass", "subrip", "hdmv_pgs_subtitle", "dvd_subtitle"] {
            let (mut info, mut request) = fixture();
            info.subtitle_tracks[0].codec_name = codec.to_string();
            info.start_time = 2.0;
            request.position = 7.375;
            request.subtitle_stream_index = Some(2);
            let spec = EncodeSpec::new("video.mkv".into(), info, request, 1).unwrap();
            let command = command(&spec, Path::new("/tmp"));
            let args: Vec<_> = command.as_std().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
            if spec.subtitle.as_ref().unwrap().is_bitmap() {
                assert!(args.iter().any(|arg| arg.contains("[1:2]setpts=PTS-7.375000/TB[subtitle];[0:0][subtitle]overlay=")));
            } else {
                assert!(args.iter().any(|arg| arg.contains("PTS+9.375000/TB,subtitles=")));
            }
        }
    }

    #[test]
    fn only_manifest_init_and_numbered_segments_are_public() {
        assert!(media_filename("segment_000001.m4s"));
        assert!(media_filename("index.m3u8"));
        for value in ["../main.db", "subtitle.ass", "probe.mp4", "segment_1.m4s", "index.m3u8.tmp"] {
            assert!(!media_filename(value));
        }
    }
}
