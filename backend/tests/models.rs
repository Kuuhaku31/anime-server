
// backend/tests/models.rs
// 验证稳定缓存身份和媒体请求契约.

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
