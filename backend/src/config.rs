//! 加载并校验应用配置.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};



#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default = "default_scan_interval")]
    pub scan_interval_seconds: u64,
    #[serde(default)]
    pub dandanplay: DandanPlayConfig,
    #[serde(default)]
    pub playback: PlaybackConfig,
}

/// 只控制资源调度和租约生命周期, 不参与播放配置的缓存身份计算.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackConfig {
    // 按目录实际字节数统计, 活动缓存受保护时允许暂时超过此淘汰目标.
    pub cache_max_bytes: u64,
    // 总进程名额包含探测和校验, 至少预留一个名额避免编码等待校验时死锁.
    pub max_processes: usize,
    // 先获得基础线程预算才启动, 目标未覆盖时最多申请到单个任务上限.
    pub ffmpeg_threads: usize,
    pub max_variant_threads: usize,
    pub max_total_ffmpeg_threads: usize,
    pub idle_timeout_seconds: u64,
    pub startup_timeout_seconds: u64,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            cache_max_bytes: 10 * 1024 * 1024 * 1024,
            max_processes: 2,
            ffmpeg_threads: 2,
            max_variant_threads: 8,
            max_total_ffmpeg_threads: 16,
            idle_timeout_seconds: 60,
            startup_timeout_seconds: 90,
        }
    }
}

impl PlaybackConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.cache_max_bytes > 0, "cache_max_bytes must be positive");
        ensure!(
            (2..=16).contains(&self.max_processes),
            "max_processes must be 2..16, including one reserved validation slot"
        );
        ensure!(
            self.ffmpeg_threads >= 1 && self.ffmpeg_threads <= self.max_variant_threads
                && self.max_variant_threads <= 32,
            "require 1 <= ffmpeg_threads <= max_variant_threads <= 32"
        );
        // 这是 H.264 编码预算, 不包含解码器, 滤镜或系统内部线程.
        ensure!(
            (self.ffmpeg_threads..=256).contains(&self.max_total_ffmpeg_threads),
            "max_total_ffmpeg_threads must be between ffmpeg_threads and 256"
        );
        ensure!(
            (20..=3600).contains(&self.idle_timeout_seconds),
            "idle_timeout_seconds must be 20..3600"
        );
        ensure!(
            (10..=600).contains(&self.startup_timeout_seconds),
            "startup_timeout_seconds must be 10..600"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebConfig {
    #[serde(default = "default_dark_mode")]
    pub default_dark_mode: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            default_dark_mode: default_dark_mode(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DandanPlayConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_base: String,
}

fn default_dark_mode() -> bool {
    true
}

fn default_scan_interval() -> u64 {
    300
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&content)?;
        config.playback.validate()?;
        ensure!(
            config.scan_interval_seconds > 0,
            "scan_interval_seconds must be positive"
        );
        Ok(config)
    }
}
