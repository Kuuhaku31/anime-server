//! 加载并校验应用配置.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppConfig {
    #[serde(default)]
    pub(crate) web: WebConfig,
    #[serde(default = "default_scan_interval")]
    pub(crate) scan_interval_seconds: u64,
    #[serde(default)]
    #[allow(dead_code)]
    dandanplay: DandanPlayConfig,
    #[serde(default)]
    pub(crate) playback: PlaybackConfig,
}

/// 只控制资源调度和租约生命周期, 不参与播放配置的缓存身份计算.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PlaybackConfig {
    // 按目录实际字节数统计, 活动缓存受保护时允许暂时超过此淘汰目标.
    pub(crate) cache_max_bytes: u64,
    // 总进程名额包含探测和校验, 至少预留一个名额避免编码等待校验时死锁.
    pub(crate) max_processes: usize,
    // 先获得基础线程预算才启动, 目标未覆盖时最多申请到单个任务上限.
    pub(crate) ffmpeg_threads: usize,
    pub(crate) max_variant_threads: usize,
    pub(crate) max_total_ffmpeg_threads: usize,
    pub(crate) idle_timeout_seconds: u64,
    pub(crate) startup_timeout_seconds: u64,
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

    /// 校验播放配置的合法性, 主要是线程预算和缓存容量.
    pub(crate) fn validate(&self) -> Result<()> {
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct WebConfig {
    #[serde(default = "default_dark_mode")]
    default_dark_mode: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            default_dark_mode: default_dark_mode(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
struct DandanPlayConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    api_base: String,
}

fn default_dark_mode() -> bool {
    true
}

fn default_scan_interval() -> u64 {
    300
}

impl AppConfig {
    pub(crate) fn load(path: &Path) -> Result<Self> {
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
