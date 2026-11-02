
// backend/src/config.rs
//! 管理运行配置的加载, 默认值和合法性校验.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

/// 直接持有应用运行所需的全部配置.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AppConfigManager {
    /// 控制前端默认是否启用深色模式.
    pub(crate) default_dark_mode: bool,
    /// 控制后台全量扫描的间隔秒数.
    pub(crate) scan_interval_seconds: u64,
    /// 控制媒体探测, 编码和校验共用的进程名额.
    pub(crate) max_processes: usize,
    /// 控制单个编码任务的基础线程数.
    pub(crate) ffmpeg_threads: usize,
    /// 控制所有编码任务共用的线程预算.
    pub(crate) max_total_ffmpeg_threads: usize,
    /// 控制媒体进程产出首个结果的超时时间秒数.
    pub(crate) startup_timeout_seconds: u64,
}

impl AppConfigManager {
    /// 从 JSON 文件加载配置并校验全部字段.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// 校验扫描周期, 并发, 线程和超时配置.
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.scan_interval_seconds > 0, "scan_interval_seconds must be positive");
        ensure!(
            (2..=16).contains(&self.max_processes),
            "max_processes must be 2..16, including one reserved validation slot"
        );
        ensure!(
            (1..=32).contains(&self.ffmpeg_threads),
            "ffmpeg_threads must be 1..32"
        );
        ensure!(
            (self.ffmpeg_threads..=256).contains(&self.max_total_ffmpeg_threads),
            "max_total_ffmpeg_threads must be between ffmpeg_threads and 256"
        );
        ensure!(
            (10..=600).contains(&self.startup_timeout_seconds),
            "startup_timeout_seconds must be 10..600"
        );
        Ok(())
    }

}

impl Default for AppConfigManager {
    /// 返回可直接启动服务的默认配置.
    fn default() -> Self {
        Self {
            default_dark_mode: true,
            scan_interval_seconds: 300,
            max_processes: 2,
            ffmpeg_threads: 2,
            max_total_ffmpeg_threads: 16,
            startup_timeout_seconds: 90,
        }
    }
}
