//! 定时执行媒体目录扫描任务.

use crate::AppState;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

// 启动后立即扫描, 每轮完成后再等待间隔, 避免慢扫描积累重叠的定时任务.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        loop {
            match crate::video::scanner::scan_once(&state).await {
                Ok(count) => info!("video scan completed: {} files", count),
                Err(e) => warn!("video scan failed: {}", e),
            }
            sleep(Duration::from_secs(state.config.scan_interval_seconds)).await;
        }
    });
}
