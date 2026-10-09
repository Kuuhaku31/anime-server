use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const DATABASE_PATH: &str = "/data/main.db";
pub const DATA_DIR: &str = "/data";
pub const VIDEO_ROOT: &str = "/media";
pub const DDP_ANI_CACHE_DIR: &str = "/data/ddp-ani";
pub const DDP_DANMAKU_CACHE_DIR: &str = "/data/ddp-danmaku";

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default = "default_scan_interval")]
    pub scan_interval_seconds: u64,
    #[serde(default)]
    pub dandanplay: DandanPlayConfig,
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
        Ok(serde_json::from_str(&content)?)
    }
}
