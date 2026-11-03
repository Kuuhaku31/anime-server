
// backend/src/config.rs
//! 管理启动配置, 环境认证参数和可原子保存的画面弹幕设置.

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::{Path, PathBuf}};
use tokio::sync::RwLock;

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
    /// 启动时加载的画面弹幕设置, 运行时值由 WebConfigManager 管理.
    #[serde(deserialize_with = "deserialize_danmaku_defaults")]
    pub(crate) danmaku: DanmakuSettings,
    /// 从环境变量读取的识别和弹幕凭据, 不参与 JSON 配置读写.
    #[serde(skip)]
    pub(crate) dandanplay: Option<DandanplayConfig>,
}

/// 描述完整的画面弹幕设置, HTTP 保存请求不允许省略字段.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DanmakuSettings {
    /// 是否在视频画面上显示已加载弹幕.
    pub(crate) enabled: bool,
    /// 使用 CSS 像素表示的弹幕字号.
    pub(crate) font_size: f64,
    /// 文字边缘描边的 CSS 像素宽度, 0 表示关闭.
    pub(crate) outline_width: f64,
    /// 填充和描边共用的不透明度.
    pub(crate) opacity: f64,
    /// 滚动弹幕从画面顶部向下可使用的高度百分比.
    pub(crate) display_area_percent: f64,
    /// 加到视频时间的秒数, 正值让弹幕提前出现.
    pub(crate) time_offset_seconds: f64,
}

/// 单独管理可热更新的 Web 设置, 不改变启动期进程和线程预算.
pub(crate) struct WebConfigManager {
    /// 已解析为绝对路径的配置文件, 原子替换始终发生在同一目录.
    path: PathBuf,
    /// 最近一次成功保存的设置, 写锁同时串行化磁盘发布.
    danmaku: RwLock<DanmakuSettings>,
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
        self.danmaku.validate()?;
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
            danmaku: DanmakuSettings::default(),
            dandanplay: None,
        }
    }
}

impl DanmakuSettings {
    /// 拒绝非有限数字和超出画面布局支持范围的设置.
    pub(crate) fn validate(&self) -> Result<()> {
        for (name, value, minimum, maximum) in [
            ("font_size", self.font_size, 12.0, 72.0),
            ("outline_width", self.outline_width, 0.0, 6.0),
            ("opacity", self.opacity, 0.1, 1.0),
            ("display_area_percent", self.display_area_percent, 10.0, 100.0),
            ("time_offset_seconds", self.time_offset_seconds, -120.0, 120.0),
        ] {
            ensure!(value.is_finite() && (minimum..=maximum).contains(&value),
                "{name} must be a finite number between {minimum} and {maximum}");
        }
        Ok(())
    }
}

impl Default for DanmakuSettings {
    /// 返回适合普通视频画面的弹幕默认样式.
    fn default() -> Self {
        Self {
            enabled: true,
            font_size: 28.0,
            outline_width: 1.5,
            opacity: 0.9,
            display_area_percent: 75.0,
            time_offset_seconds: 0.0,
        }
    }
}

impl WebConfigManager {
    /// 从已校验的启动配置建立独立的运行时弹幕快照.
    pub(crate) fn new(path: &Path, config: &AppConfigManager) -> Result<Self> {
        Ok(Self {
            path: fs::canonicalize(path)?,
            danmaku: RwLock::new(config.danmaku.clone()),
        })
    }

    /// 读取最近一次成功保存的弹幕设置, 不公开其他配置.
    pub(crate) async fn danmaku(&self) -> DanmakuSettings {
        self.danmaku.read().await.clone()
    }

    /// 先原子保存完整配置再发布运行时值, 失败时保持旧快照.
    pub(crate) async fn save_danmaku(
        &self, settings: DanmakuSettings, startup: &AppConfigManager,
    ) -> Result<DanmakuSettings> {
        settings.validate()?;
        let mut current = self.danmaku.write().await;
        let mut config = startup.clone();
        config.danmaku = settings;
        let mut content = serde_json::to_vec_pretty(&config)?;
        content.push(b'\n');
        let directory = self.path.parent().context("config directory is missing")?;
        let name = self.path.file_name().and_then(|name| name.to_str())
            .context("config filename must be valid UTF-8")?;
        // 原子替换只要求目录可写, 这里另外尊重现有文件的写权限.
        drop(tokio::fs::OpenOptions::new().write(true).open(&self.path).await?);
        crate::utils::atomic_file(directory, name, &content).await
            .context("cannot save danmaku settings")?;
        *current = config.danmaku;
        Ok(current.clone())
    }
}

impl std::fmt::Debug for DandanplayConfig {
    /// 调试输出隐藏认证信息, 避免日志意外泄露密钥.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DandanplayConfig").finish_non_exhaustive()
    }
}

/// 仅在配置文件读取时补齐缺省字段, HTTP 请求仍使用严格的完整结构.
fn deserialize_danmaku_defaults<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<DanmakuSettings, D::Error> {
    let overrides = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
    let serde_json::Value::Object(mut fields) = serde_json::to_value(DanmakuSettings::default())
        .map_err(serde::de::Error::custom)? else {
            return Err(serde::de::Error::custom("danmaku defaults must be an object"));
        };
    fields.extend(overrides);
    serde_json::from_value(serde_json::Value::Object(fields)).map_err(serde::de::Error::custom)
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
