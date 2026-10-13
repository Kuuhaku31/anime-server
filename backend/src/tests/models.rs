//! 校验播放请求选轨语义和播放配置缓存身份的测试.

use md5::{Digest, Md5};

use crate::{constants::ENCODER_PROFILE, process::testing::{fixture, FILE_ID}};

use super::*;

// 缺省字段使用服务端默认轨道, 显式 null 关闭对应轨道, 历史进度不进入配置.
#[test]
fn defaults_explicit_off_and_saved_positions_are_distinct() {
    let (info, request) = fixture();
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &request.with_saved_position(Some(7.375))).unwrap();
    assert_eq!(config.selection(), (Some(1), None));
    assert_eq!(config, PlaybackVariantConfig::new(FILE_ID, &info, &PlaybackRequest::default()).unwrap());
    let request: PlaybackRequest = serde_json::from_str(
        r#"{"audio_stream_index":null,"subtitle_stream_index":2,"position":8.5}"#).unwrap();
    assert_eq!(request.position, Some(8.5));
    let changed = PlaybackVariantConfig::new(FILE_ID, &info, &request.with_saved_position(Some(3.0))).unwrap();
    assert_eq!(changed.selection(), (None, Some(2)));
    assert_ne!(changed.variant_key(), config.variant_key());
}

// 数据库偏好只补充缺省字段, 不能覆盖请求中显式选择或关闭的轨道.
#[test]
fn saved_tracks_fill_only_omitted_fields() {
    let omitted: PlaybackRequest = serde_json::from_str("{}").unwrap();
    let resolved = omitted.with_preferences(Some(4), Some(7));
    assert_eq!(resolved.audio_stream_index, Some(Some(4)));
    assert_eq!(resolved.subtitle_stream_index, Some(Some(7)));

    let (info, _) = fixture();
    let chosen = PlaybackRequest::default().with_preferences(Some(1), Some(2));
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &chosen).unwrap();
    assert_eq!(config.selection(), (Some(1), Some(2)));

    let explicit: PlaybackRequest = serde_json::from_str(r#"{"audio_stream_index":null}"#).unwrap();
    let resolved = explicit.with_preferences(Some(4), Some(2));
    assert_eq!(resolved.audio_stream_index, Some(None));
    assert_eq!(resolved.subtitle_stream_index, Some(Some(2)));
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &resolved).unwrap();
    assert_eq!(config.selection(), (None, Some(2)));
}

// 未指定轨道时优先默认音轨, 字幕选择默认或强制轨道.
#[test]
fn initial_track_preferences_match_server_defaults() {
    let info: MediaInfo = serde_json::from_value(serde_json::json!({
        "duration": 20.0, "start_time": 0.0,
        "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
        "audio_tracks": [
            {"stream_index": 1, "codec_name": "aac", "is_default": false, "is_forced": false},
            {"stream_index": 4, "codec_name": "aac", "is_default": true, "is_forced": false}
        ],
        "subtitle_tracks": [
            {"stream_index": 3, "codec_name": "ass", "is_default": false, "is_forced": true}
        ],
        "attachments": []
    })).unwrap();
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &PlaybackRequest::default()).unwrap();
    assert_eq!(config.selection(), (Some(4), Some(3)));
}

// 缓存身份按字段声明顺序序列化并保留 null, 运行时字段不参与哈希.
#[test]
fn configuration_serialization_is_canonical_and_excludes_runtime_fields() {
    let (info, _) = fixture();
    let request: PlaybackRequest = serde_json::from_str(r#"{"subtitle_stream_index":null}"#).unwrap();
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &request).unwrap();
    let canonical = format!(
        r#"{{"src_file_pre16mib_md5":"{FILE_ID}","video_stream":0,"audio_stream":1,"subtitle_stream":null,"encoder_profile":"{ENCODER_PROFILE}"}}"#
    );
    assert_eq!(serde_json::to_string(&config).unwrap(), canonical);
    assert_eq!(config.variant_key(), format!("{:x}", Md5::digest(canonical.as_bytes())));
    assert!(crate::utils::check_md5_ok(config.variant_key()));
    // 非法源文件 ID 不能构造配置, 避免把任意字符串写入缓存目录名.
    for file_id in ["../main.db", "g".repeat(32).as_str(), "a", ""] {
        assert!(PlaybackVariantConfig::new(file_id, &info, &request).is_err());
    }
}
