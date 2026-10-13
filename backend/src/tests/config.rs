//! 校验播放配置默认值和资源预算约束的测试.

use super::*;

// 缓存容量默认 10 GiB, 只接受正整数字节数.
#[test]
fn cache_capacity_is_a_positive_byte_count_with_a_ten_gib_default() {
    let config: PlaybackConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.cache_max_bytes, 10_737_418_240);
    assert_eq!(config.max_variant_threads, 8);
    assert_eq!(config.max_total_ffmpeg_threads, 16);
    assert!(config.validate().is_ok());
    let config: PlaybackConfig = serde_json::from_str(r#"{"cache_max_bytes":0}"#).unwrap();
    assert!(config.validate().is_err());
    for value in ["-1", "1.5", "\"10GiB\""] {
        assert!(serde_json::from_str::<PlaybackConfig>(&format!(
            r#"{{"cache_max_bytes":{value}}}"#
        ))
        .is_err());
    }

}

// 进程数, 单任务线程和总线程预算越界时配置校验失败.
#[test]
fn encoder_budgets_and_the_validation_slot_are_required() {
    // 非法预算必须在启动时拒绝, 不能让加权许可永久等待.
    for value in [
        r#"{"max_processes":1}"#, r#"{"ffmpeg_threads":0}"#,
        r#"{"ffmpeg_threads":9}"#, r#"{"max_variant_threads":33}"#,
        r#"{"max_total_ffmpeg_threads":1}"#, r#"{"max_total_ffmpeg_threads":257}"#,
    ] {
        assert!(serde_json::from_str::<PlaybackConfig>(value).unwrap().validate().is_err());
    }
}
