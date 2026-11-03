
// backend/src/config.rs
//! 管理 JSON 运行配置和环境认证参数的加载与校验.

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::Path};

#[cfg(test)]
#[path = "../tests/config.rs"]
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
    /// 从环境变量读取的识别和弹幕凭据, 不参与 JSON 配置读写.
    #[serde(skip)]
    pub(crate) dandanplay: Option<DandanplayConfig>,
}

/// 保存仅从服务端环境获取的 DandanPlay 应用认证信息.
#[derive(Clone)]
pub(crate) struct DandanplayConfig {
    /// 由 DandanPlay 分配的应用标识.
    pub(crate) app_id: String,
    /// 用于生成请求签名的应用密钥, 不发送给浏览器或上游接口.
    pub(crate) app_secret: String,
}

impl AppConfigManager {
    /// 从 JSON 文件读取运行配置, 从环境变量读取认证参数并统一校验.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let mut config: Self = serde_json::from_str(&content)?;
        let app_id = optional_environment("DANDAN_APP_ID")?;
        let app_secret = optional_environment("DANDAN_APP_SECRET")?;
        config.dandanplay = match (app_id, app_secret) {
            (None, None) => None,
            (Some(app_id), Some(app_secret)) => Some(DandanplayConfig { app_id, app_secret }),
            _ => bail!("DANDAN_APP_ID and DANDAN_APP_SECRET must be set together"),
        };
        config.validate()?;
        Ok(config)
    }

    /// 校验扫描周期, 并发, 线程, 超时及可选 DandanPlay 凭据.
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
        if let Some(config) = &self.dandanplay {
            ensure!(!config.app_id.trim().is_empty()
                && reqwest::header::HeaderValue::from_str(&config.app_id).is_ok(),
                "DANDAN_APP_ID must be a non-empty HTTP header value");
            ensure!(!config.app_secret.trim().is_empty(), "DANDAN_APP_SECRET must not be blank");
        }
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
            dandanplay: None,
        }
    }
}

impl std::fmt::Debug for DandanplayConfig {
    /// 调试输出隐藏认证信息, 避免日志意外泄露密钥.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DandanplayConfig").finish_non_exhaustive()
    }
}

/// 将未设置或空字符串视为未配置, 编码错误只报告变量名而不回显凭据.
fn optional_environment(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} must contain valid UTF-8"),
    }
}
