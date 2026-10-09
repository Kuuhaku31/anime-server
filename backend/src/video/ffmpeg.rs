use std::{io::{Read, Seek, SeekFrom}, path::{Path, PathBuf}, process::Stdio};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tokio::{process::Command, sync::watch};

use super::{assets::RenderAssets, cache::{is_key, PlaybackVariantConfig, ENCODER_PROFILE},
    metadata::{Attachment, MediaInfo, Track}, process::checked_output_cancellable};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackRequest {
    pub video_stream_index: Option<i32>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    #[serde(default)]
    pub position: f64,
}

pub struct EncodeSpec {
    pub config: PlaybackVariantConfig,
    pub source: PathBuf,
    pub duration: f64,
    pub source_start: f64,
    pub subtitle: Option<Track>,
    pub attachments: Vec<Attachment>,
    pub hdr: bool,
    pub threads: usize,
}

impl EncodeSpec {
    pub fn new(id: &str, source: PathBuf, info: MediaInfo, request: PlaybackRequest, threads: usize) -> Result<Self> {
        let id = id.to_ascii_lowercase();
        ensure!(is_key(&id), "invalid source file ID");
        ensure!(request.position.is_finite() && request.position >= 0.0 && request.position < info.duration,
            "position must be within the video duration");
        let video = match request.video_stream_index {
            Some(index) => info.video_tracks.iter().find(|track| track.stream_index == index),
            None => info.video_tracks.first(),
        }.context("selected video track does not exist")?;
        let audio = select(&info.audio_tracks, request.audio_stream_index, "audio")?;
        let subtitle = select(&info.subtitle_tracks, request.subtitle_stream_index, "subtitle")?;
        Ok(Self {
            config: PlaybackVariantConfig {
                src_file_pre16mib_md5: id, video_stream: video.stream_index,
                audio_stream: audio.map(|track| track.stream_index),
                subtitle_stream: subtitle.as_ref().map(|track| track.stream_index),
                encoder_profile: ENCODER_PROFILE.to_string(),
            },
            source, duration: info.duration, source_start: info.start_time,
            hdr: video.is_hdr(), subtitle, attachments: info.attachments, threads,
        })
    }
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}

pub fn command(spec: &EncodeSpec, directory: &Path, assets: Option<&RenderAssets>) -> Result<Command> {
    let mut command = Command::new("ffmpeg");
    command.current_dir(directory).kill_on_drop(true)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.args(["-nostdin", "-hide_banner", "-v", "warning", "-y",
        "-threads", &spec.threads.to_string(), "-filter_threads", "1", "-filter_complex_threads", "1"]);
    command.arg("-i").arg(&spec.source);
    let bitmap = spec.subtitle.as_ref().filter(|track| track.is_bitmap());
    let mut filters = Vec::new();
    if spec.hdr {
        filters.push("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv".to_string());
    }
    filters.push("scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2".to_string());
    filters.push("format=yuv420p".to_string());
    // Preserve timestamps and allow one frame per 60 Hz slot, including rounded Matroska timestamps.
    filters.push("select='isnan(prev_selected_t)+gt(floor(t*60+0.5),floor(prev_selected_t*60+0.5))'".to_string());
    if spec.subtitle.as_ref().is_some_and(|track| !track.is_bitmap()) {
        let assets = assets.context("text subtitle resources are missing")?;
        // Extracted subtitles retain the container's original timestamp offset.
        filters.push(format!("setpts=PTS+{:.6}/TB,{},setpts=PTS-STARTPTS", spec.source_start, assets.filter()));
    }
    if let Some(track) = bitmap {
        command.args(["-filter_complex",
            &format!("[0:{}][0:{}]overlay=eof_action=pass,{}[video]",
                spec.config.video_stream, track.stream_index, filters.join(",")),
            "-map", "[video]"]);
    } else {
        command.args(["-map", &format!("0:{}", spec.config.video_stream), "-vf", &filters.join(",")]);
    }
    command.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
        "-pix_fmt", "yuv420p", "-profile:v", "high", "-threads", &spec.threads.to_string(),
        "-bf", "0", "-force_key_frames", "expr:gte(t,n_forced*4)", "-sc_threshold", "0",
        "-fpsmax", "60", "-fps_mode", "passthrough"]);
    if spec.hdr { command.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]); }
    if let Some(index) = spec.config.audio_stream {
        command.args(["-map", &format!("0:{index}"), "-c:a", "aac", "-profile:a", "aac_low",
            "-ac", "2", "-ar", "48000", "-b:a", "192k"]);
    } else { command.arg("-an"); }
    // Keep cached media time aligned with source time across native HLS and MSE.
    command.args(["-sn", "-dn", "-map_metadata", "-1", "-avoid_negative_ts", "disabled",
        "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", "4",
        "-hls_list_size", "0", "-hls_playlist_type", "event",
        "-hls_flags", "independent_segments+temp_file", "-hls_fmp4_init_filename", "init.mp4",
        "-hls_segment_filename", "segment_%06d.m4s", "index.m3u8"]);
    Ok(command)
}

pub struct Playlist {
    pub segments: Vec<String>,
    pub duration: f64,
    pub complete: bool,
}

pub async fn read_playlist(directory: &Path) -> Result<Option<Playlist>> {
    let content = match tokio::fs::read_to_string(directory.join("index.m3u8")).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(parse_playlist(&content)?))
}

fn parse_playlist(content: &str) -> Result<Playlist> {
    ensure!(content.starts_with("#EXTM3U\n") && content.contains("#EXT-X-MAP:URI=\"init.mp4\""),
        "invalid fMP4 playlist");
    let mut playlist = Playlist { segments: Vec::new(), duration: 0.0, complete: false };
    let mut pending = false;
    for line in content.lines().filter(|line| !line.is_empty()) {
        ensure!(!playlist.complete, "data follows the HLS end marker");
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            ensure!(!pending, "HLS segment URI is missing");
            let duration = value.split(',').next().context("invalid HLS duration")?.parse::<f64>()?;
            ensure!(duration.is_finite() && duration > 0.0, "invalid HLS segment duration");
            playlist.duration += duration;
            pending = true;
        } else if line == "#EXT-X-ENDLIST" {
            playlist.complete = true;
        } else if !line.starts_with('#') {
            ensure!(pending && line == format!("segment_{:06}.m4s", playlist.segments.len()),
                "invalid or nonsequential HLS segment URI");
            ensure!(media_filename(line), "unsafe HLS segment URI");
            playlist.segments.push(line.to_string());
            pending = false;
        } else {
            ensure!(matches!(line, "#EXTM3U" | "#EXT-X-INDEPENDENT-SEGMENTS"
                | "#EXT-X-MEDIA-SEQUENCE:0" | "#EXT-X-PLAYLIST-TYPE:EVENT" | "#EXT-X-MAP:URI=\"init.mp4\"")
                || line.strip_prefix("#EXT-X-TARGETDURATION:").is_some_and(|v| v.parse::<u32>().is_ok())
                || line.strip_prefix("#EXT-X-VERSION:").is_some_and(|v| v.parse::<u32>().is_ok()),
                "unsupported HLS tag: {line}");
        }
    }
    ensure!(!pending && playlist.duration.is_finite(), "incomplete HLS playlist");
    Ok(playlist)
}

pub fn media_filename(name: &str) -> bool {
    name == "index.m3u8" || name == "init.mp4"
        || name.strip_prefix("segment_").and_then(|s| s.strip_suffix(".m4s"))
            .is_some_and(|s| s.len() == 6 && s.bytes().all(|c| c.is_ascii_digit()))
}

pub async fn validate_first_segment(directory: &Path, audio: Option<bool>,
    cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let mut command = Command::new("ffprobe");
    command.current_dir(directory).args(["-v", "error", "-show_entries",
        "stream=codec_type,codec_name,pix_fmt,profile,channels,sample_rate", "-of", "json",
        "concat:init.mp4|segment_000000.m4s"]);
    let result = checked_output_cancellable(&mut command, 15, cancel).await?;
    #[derive(Deserialize)]
    struct Output { streams: Vec<Stream> }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: String, codec_name: String, pix_fmt: Option<String>, profile: Option<String>,
        channels: Option<u32>, sample_rate: Option<String>,
    }
    let output: Output = serde_json::from_slice(&result)?;
    let has_audio = output.streams.iter().any(|s| s.codec_type == "audio");
    ensure!(audio.is_none_or(|expected| expected == has_audio), "unexpected HLS audio selection");
    ensure!(output.streams.len() == if has_audio { 2 } else { 1 }, "unexpected HLS stream count");
    let video = output.streams.iter().find(|s| s.codec_type == "video").context("output has no video")?;
    ensure!(video.codec_name == "h264" && video.pix_fmt.as_deref() == Some("yuv420p")
        && video.profile.as_deref() == Some("High"), "output is not 8-bit H.264 High");
    if has_audio {
        let audio = output.streams.iter().find(|s| s.codec_type == "audio").context("output has no audio")?;
        ensure!(audio.codec_name == "aac" && audio.profile.as_deref() == Some("LC")
            && audio.channels == Some(2) && audio.sample_rate.as_deref() == Some("48000"),
            "output is not stereo AAC-LC at 48 kHz");
    }
    Ok(())
}

pub async fn validate_final_playlist(directory: &Path) -> Result<f64> {
    let playlist = read_playlist(directory).await?.context("HLS playlist is missing")?;
    ensure!(playlist.complete && !playlist.segments.is_empty(), "HLS output is incomplete");
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        regular_file(&directory.join("index.m3u8"))?;
        validate_mp4(&directory.join("init.mp4"), [*b"ftyp", *b"moov"])?;
        for segment in &playlist.segments {
            validate_mp4(&directory.join(segment), [*b"moof", *b"mdat"])?;
        }
        Ok(playlist.duration)
    }).await.context("HLS validation task failed")?
}

fn regular_file(path: &Path) -> Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(metadata.is_file() && metadata.len() > 0, "missing, empty or unsafe HLS file: {}", path.display());
    Ok(metadata.len())
}

fn validate_mp4(path: &Path, required: [[u8; 4]; 2]) -> Result<()> {
    let length = regular_file(path)?;
    let mut file = std::fs::File::open(path)?;
    let mut position = 0;
    let mut found = [false; 2];
    while position < length {
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header[..4].try_into()?) as u64;
        let (size, header_size) = if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended)?;
            (u64::from_be_bytes(extended), 16)
        } else { (if size == 0 { length - position } else { size }, 8) };
        ensure!(size >= header_size && size <= length - position, "truncated MP4 box: {}", path.display());
        for (index, kind) in required.iter().enumerate() {
            if header[4..] == *kind && size > header_size { found[index] = true; }
        }
        position += size;
        file.seek(SeekFrom::Start(position))?;
    }
    ensure!(found.into_iter().all(|found| found), "missing required MP4 boxes: {}", path.display());
    Ok(())
}

#[cfg(test)]
pub fn fixture() -> (MediaInfo, PlaybackRequest) {
    let info = serde_json::from_value(serde_json::json!({
        "duration": 20.0, "start_time": 0.0,
        "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
        "audio_tracks": [{"stream_index": 1, "codec_name": "aac", "is_default": true, "is_forced": false}],
        "subtitle_tracks": [{"stream_index": 2, "codec_name": "ass", "is_default": false, "is_forced": false}],
        "attachments": []
    })).unwrap();
    let request = PlaybackRequest {
        video_stream_index: None,
        audio_stream_index: Some(1), subtitle_stream_index: None, position: 0.0,
    };
    (info, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_tracks_positions_and_client_profiles_fail() {
        for position in [-1.0, f64::NAN, f64::INFINITY, 20.0] {
            let (info, mut request) = fixture();
            request.position = position;
            assert!(EncodeSpec::new(&"a".repeat(32), "video".into(), info, request, 1).is_err());
        }
        for field in ["video_stream_index", "audio_stream_index", "subtitle_stream_index"] {
            let (info, _) = fixture();
            let mut value = serde_json::json!({});
            value[field] = 99.into();
            let request = serde_json::from_value(value).unwrap();
            assert!(EncodeSpec::new(&"a".repeat(32), "video".into(), info, request, 1).is_err());
        }
        assert!(serde_json::from_value::<PlaybackRequest>(
            serde_json::json!({"encoder_profile": ENCODER_PROFILE})).is_err());
    }

    #[test]
    fn positions_do_not_change_the_cache_or_encoding_start() {
        let (info, mut request) = fixture();
        request.position = 7.375;
        let spec = EncodeSpec::new(&"a".repeat(32), "video.mp4".into(), info, request, 2).unwrap();
        let (info, request) = fixture();
        let from_zero = EncodeSpec::new(&"a".repeat(32), "other-path.mp4".into(), info, request, 2).unwrap();
        assert_eq!(spec.config, from_zero.config);
        let command = command(&spec, Path::new("/tmp"), None).unwrap();
        let args: Vec<_> = command.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        for pair in [["-c:v", "libx264"], ["-pix_fmt", "yuv420p"], ["-profile:v", "high"],
            ["-c:a", "aac"], ["-hls_segment_type", "fmp4"], ["-bf", "0"],
            ["-fps_mode", "passthrough"], ["-avoid_negative_ts", "disabled"]] {
            assert!(args.windows(2).any(|args| args == pair));
        }
        assert!(!args.contains(&"copy"));
        assert!(!args.contains(&"-ss"));
        assert!(!args.iter().any(|arg| arg.contains("subtitles=")));
    }

    #[tokio::test]
    async fn text_and_bitmap_subtitles_are_burned() {
        let root = std::env::temp_dir().join(format!("anime-assets-{}", super::super::cache::random_id().unwrap()));
        tokio::fs::create_dir_all(root.join("fonts")).await.unwrap();
        tokio::fs::create_dir(root.join("subtitles")).await.unwrap();
        let assets = RenderAssets::allocate(&root).await.unwrap();
        for codec in ["ass", "subrip", "hdmv_pgs_subtitle", "dvd_subtitle"] {
            let (mut info, mut request) = fixture();
            info.subtitle_tracks[0].codec_name = codec.to_string();
            info.start_time = 2.0;
            request.position = 7.375;
            request.subtitle_stream_index = Some(2);
            let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, 1).unwrap();
            let command = command(&spec, Path::new("/tmp"), Some(&assets)).unwrap();
            let args: Vec<_> = command.as_std().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
            if spec.subtitle.as_ref().unwrap().is_bitmap() {
                assert!(args.iter().any(|arg| arg.contains("[0:0][0:2]overlay=")));
            } else {
                assert!(args.iter().any(|arg| arg.contains("PTS+2.000000/TB,subtitles=")));
            }
        }
        assets.cleanup().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn only_manifest_init_and_numbered_segments_are_public() {
        assert!(media_filename("segment_000001.m4s"));
        assert!(media_filename("index.m3u8"));
        for value in ["../main.db", "subtitle.ass", "probe.mp4", "segment_1.m4s", "index.m3u8.tmp"] {
            assert!(!media_filename(value));
        }
    }

    #[test]
    fn playlists_require_finite_durations_and_local_sequential_fragments() {
        let manifest = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.000000,\nsegment_000000.m4s\n#EXT-X-ENDLIST\n";
        let playlist = parse_playlist(manifest).unwrap();
        assert!(playlist.complete);
        assert_eq!(playlist.duration, 4.0);
        assert_eq!(playlist.segments, ["segment_000000.m4s"]);
        assert!(!parse_playlist(&manifest.replace("#EXT-X-ENDLIST\n", "")).unwrap().complete);
        for invalid in [
            manifest.replace("4.000000", "NaN"),
            manifest.replace("4.000000", "0"),
            manifest.replace("segment_000000.m4s", "../segment_000000.m4s"),
            manifest.replace("segment_000000.m4s", "https://example.invalid/segment_000000.m4s"),
            manifest.replace("segment_000000.m4s", "segment_000001.m4s"),
            manifest.replace("segment_000000.m4s\n", ""),
            manifest.replace("#EXT-X-MAP:URI=\"init.mp4\"\n", ""),
            format!("{manifest}#EXTINF:4,\nsegment_000001.m4s\n"),
        ] {
            assert!(parse_playlist(&invalid).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn completed_playlists_reject_missing_or_truncated_fragments() {
        let root = std::env::temp_dir().join(format!("anime-fragments-{}", super::super::cache::random_id().unwrap()));
        tokio::fs::create_dir(&root).await.unwrap();
        let boxes = |kinds: [[u8; 4]; 2]| {
            let mut bytes = Vec::new();
            for kind in kinds {
                bytes.extend_from_slice(&12_u32.to_be_bytes());
                bytes.extend_from_slice(&kind);
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes
        };
        let fragment = boxes([*b"moof", *b"mdat"]);
        tokio::fs::write(root.join("index.m3u8"),
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nsegment_000000.m4s\n#EXT-X-ENDLIST\n").await.unwrap();
        tokio::fs::write(root.join("init.mp4"), boxes([*b"ftyp", *b"moov"])).await.unwrap();
        assert!(validate_final_playlist(&root).await.is_err());
        tokio::fs::write(root.join("segment_000000.m4s"), &fragment).await.unwrap();
        assert_eq!(validate_final_playlist(&root).await.unwrap(), 4.0);
        tokio::fs::write(root.join("segment_000000.m4s"), &fragment[..fragment.len() - 1]).await.unwrap();
        assert!(validate_final_playlist(&root).await.is_err());
        tokio::fs::write(root.join("segment_000000.m4s"), boxes([*b"ftyp", *b"moov"])).await.unwrap();
        assert!(validate_final_playlist(&root).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
