
// backend/src/tests/models.rs
//! 验证稳定缓存身份和公开媒体契约.

use super::*;

/// 身份描述保留字段顺序和显式 null, 不同视频流或版本必须隔离缓存.
#[test]
fn cache_descriptor_round_trips_and_keeps_ordered_identity() {
    let spec = crate::testing::spec();
    let expected = format!(
        "{{\"media_id\":\"{}\",\"video_stream_index\":0,\"audio_stream_index\":1,\"subtitle_stream_index\":null,\"version\":\"{}\"}}",
        "a".repeat(32), crate::constants::CACHE_VERSION,
    );
    assert_eq!(spec.descriptor().unwrap(), expected.as_bytes());
    assert_eq!(CacheSpec::from_descriptor(expected.as_bytes()).unwrap(), spec);
    let mut changed = spec.clone();
    changed.video_stream_index = 4;
    assert_ne!(changed.handle_id().unwrap(), spec.handle_id().unwrap());
    changed = spec.clone();
    changed.audio_stream_index = None;
    assert_ne!(changed.handle_id().unwrap(), spec.handle_id().unwrap());
    changed = spec.clone();
    changed.version.push_str("-next");
    assert_ne!(changed.handle_id().unwrap(), spec.handle_id().unwrap());
}

/// 前端只能接收 metadata 和 configs, 内部时间轴和附件不泄露.
#[test]
fn media_response_keeps_internal_probe_details_private() {
    let media = MediaInfo { metadata: crate::testing::metadata(), configs: MediaConfigs::default() };
    let json = serde_json::to_value(media).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 2);
    assert!(json["metadata"].get("video_streams").is_some());
    assert!(json["metadata"].get("start_time").is_none());
    assert!(json["metadata"].get("attachments").is_none());
    assert!(json["metadata"]["video_streams"][0].get("color_transfer").is_none());
}
