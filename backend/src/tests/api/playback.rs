
// backend/src/tests/api/playback.rs
//! 验证直接选轨, null 关闭语义和最小播放请求.

use super::*;

/// 缺省继承配置, 显式 null 关闭, 三类流都按全局索引校验.
#[test]
fn selected_streams_preserve_defaults_and_explicit_disabled_choices() {
    let metadata = crate::testing::metadata();
    let configs = MediaConfigs { position: Some(7.375), video_stream_index: Some(4),
        audio_stream_index: Some(3), subtitle_stream_index: Some(2) };
    let query: PlaybackQuery = serde_json::from_value(serde_json::json!({"path": "/media/episode.mkv"})).unwrap();
    let spec = select_streams("a".repeat(32), &query, &metadata, &configs).unwrap();
    assert_eq!((spec.video_stream_index, spec.audio_stream_index, spec.subtitle_stream_index), (4, Some(3), Some(2)));
    let query: PlaybackQuery = serde_json::from_value(serde_json::json!({
        "path": "/media/episode.mkv", "video_stream_index": 0, "audio_stream_index": null, "subtitle_stream_index": null,
    })).unwrap();
    let spec = select_streams("a".repeat(32), &query, &metadata, &configs).unwrap();
    assert_eq!((spec.video_stream_index, spec.audio_stream_index, spec.subtitle_stream_index), (0, None, None));
}

/// 拒绝不存在或类别不匹配的索引, 不允许客户端传入等待位置和缓存身份.
#[test]
fn invalid_indices_and_removed_playback_fields_are_rejected() {
    let metadata = crate::testing::metadata();
    for field in ["video_stream_index", "audio_stream_index", "subtitle_stream_index"] {
        let query: PlaybackQuery = serde_json::from_value(serde_json::json!({
            "path": "/media/episode.mkv", field: 99,
        })).unwrap();
        assert_eq!(select_streams("a".repeat(32), &query, &metadata, &MediaConfigs::default())
            .unwrap_err().status, StatusCode::BAD_REQUEST);
    }
    for field in ["position", "media_id", "handle_id", "version"] {
        assert!(serde_json::from_value::<PlaybackQuery>(serde_json::json!({
            "path": "/media/episode.mkv", field: 1,
        })).is_err());
    }
}
