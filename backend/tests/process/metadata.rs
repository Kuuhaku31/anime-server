
// backend/tests/process/metadata.rs
//! 验证探测元数据归一化和不完整媒体错误.

use super::*;

/// 正起点 Matroska 时长归一化后, 视频索引仍保持容器内全局索引.
#[test]
fn probe_normalizes_timestamps_and_excludes_cover_art() -> Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "format": {"duration": "12.5", "start_time": "2.5", "format_name": "matroska,webm"},
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}},
            {"index": 4, "codec_type": "video", "codec_name": "hevc", "color_transfer": "smpte2084"},
            {"index": 7, "codec_type": "audio", "codec_name": "aac", "tags": {"language": "jpn"}},
        ],
    }))?;
    let metadata = parse_probe(&bytes)?;
    assert_eq!(metadata.duration, 10.0);
    assert_eq!(metadata.video_streams.len(), 1);
    assert_eq!(metadata.video_streams[0].stream_index, 4);
    assert!(metadata.video_streams[0].is_hdr());
    assert_eq!(metadata.audio_streams[0].language.as_deref(), Some("jpn"));
    assert!(parse_probe(br#"{"format":{"duration":"NaN","format_name":"mp4"},"streams":[]}"#).is_err());
    Ok(())
}
