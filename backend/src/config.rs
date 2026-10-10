//! 加载并校验应用配置.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const DATABASE_PATH: &str = "/data/main.db";
pub const DATA_DIR: &str = "/data";
pub const VIDEO_ROOT: &str = "/media";
pub const DDP_ANI_CACHE_DIR: &str = "/data/ddp-ani";
pub const DDP_DANMAKU_CACHE_DIR: &str = "/data/ddp-danmaku";
pub const CACHE_DIR: &str = "/cache";
pub const FALLBACK_FONT: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";

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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackConfig {
    pub cache_max_bytes: u64,
    pub max_processes: usize,
    pub ffmpeg_threads: usize,
    pub idle_timeout_seconds: u64,
    pub startup_timeout_seconds: u64,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            cache_max_bytes: 10 * 1024 * 1024 * 1024,
            max_processes: 2,
            ffmpeg_threads: 2,
            idle_timeout_seconds: 60,
            startup_timeout_seconds: 90,
        }
    }
}

impl PlaybackConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.cache_max_bytes > 0, "cache_max_bytes must be positive");
        ensure!(
            (1..=16).contains(&self.max_processes),
            "max_processes must be 1..16"
        );
        ensure!(
            (1..=32).contains(&self.ffmpeg_threads),
            "ffmpeg_threads must be 1..32"
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
