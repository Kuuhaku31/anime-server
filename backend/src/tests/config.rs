
// backend/src/tests/config.rs
//! 验证扁平配置以及进程和线程预算约束.

use super::*;

/// 新配置只接受直接字段, 旧缓存容量和租约设置必须明确拒绝.
#[test]
fn flat_config_rejects_removed_settings_and_invalid_budgets() {
    let config: AppConfigManager = serde_json::from_str("{}").unwrap();
    config.validate().unwrap();
    for value in [
        serde_json::json!({"web": {}}),
        serde_json::json!({"cache_max_bytes": 1}),
        serde_json::json!({"idle_timeout_seconds": 60}),
        serde_json::json!({"max_variant_threads": 8}),
    ] {
        assert!(serde_json::from_value::<AppConfigManager>(value).is_err());
    }
    for value in [
        serde_json::json!({"max_processes": 1}),
        serde_json::json!({"ffmpeg_threads": 0}),
        serde_json::json!({"ffmpeg_threads": 4, "max_total_ffmpeg_threads": 3}),
        serde_json::json!({"scan_interval_seconds": 0}),
    ] {
        assert!(serde_json::from_value::<AppConfigManager>(value).unwrap().validate().is_err());
    }
}
