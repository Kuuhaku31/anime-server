//! 校验 FFmpeg 命令参数, 播放请求和 HLS 清单解析的测试.

use crate::{
    constants::ENCODER_PROFILE,
    models::{MediaInfo, PlaybackRequest, PlaybackVariantConfig},
    process::testing::fixture,
};

use super::*;

// 按会话创建流程构造执行参数, 历史进度只应影响会话目标而不进入配置.
fn make_spec(file_id: &str, source: &str, info: MediaInfo, request: PlaybackRequest, saved: Option<f64>) -> anyhow::Result<EncodeSpec> {
    let request = request.with_saved_position(saved);
    let config = PlaybackVariantConfig::new(file_id, &info, &request)?;
    EncodeSpec::from_config(config, source.into(), info)
}

// 客户端不能提交服务端字段, 越界轨道必须在构造配置时拒绝.
#[test]
fn invalid_tracks_and_client_profiles_fail() {
    for value in [
        serde_json::json!({"video_stream_index":0}), serde_json::json!({"encoder_profile":ENCODER_PROFILE}),
        serde_json::json!({"variant_key":"a".repeat(32)}), serde_json::json!({"source_version":"old"}),
    ] {
        assert!(serde_json::from_value::<PlaybackRequest>(value).is_err());
    }
    for field in ["audio_stream_index", "subtitle_stream_index"] {
        let (info, _) = fixture();
        let request = serde_json::from_value(serde_json::json!({field:99})).unwrap();
        assert!(make_spec(&"a".repeat(32), "video", info, request, None).is_err());
    }
}

// 位置, 线程和路径只影响命令参数, 不改变缓存身份.
#[test]
fn positions_threads_and_paths_do_not_enter_the_configuration() {
    let (info, request) = fixture();
    let spec = make_spec(&"a".repeat(32), "video.mp4", info.clone(), request, Some(7.375)).unwrap();
    let zero = make_spec(&"a".repeat(32), "other.mp4", info, PlaybackRequest::default(), None).unwrap();
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

// 清单只接受本地连续分片和有限时长, 资源名拒绝路径穿越与内部文件.
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
