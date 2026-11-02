
// backend/src/services.rs
//! 按依赖顺序初始化和读取进程级唯一 Manager.

use std::{path::Path, sync::OnceLock};

use anyhow::{anyhow, Result};

use crate::{
    cache::CacheManager, config::AppConfigManager, constants,
    db::DatabaseManager, process::ProcessManager, scanner::ScannerManager,
};

/// 全局应用配置实例.
static CONFIG: OnceLock<AppConfigManager> = OnceLock::new();
/// 全局数据库实例.
static DATABASE: OnceLock<DatabaseManager> = OnceLock::new();
/// 全局媒体扫描实例.
static SCANNER: OnceLock<ScannerManager> = OnceLock::new();
/// 全局缓存实例.
static CACHE: OnceLock<CacheManager> = OnceLock::new();
/// 全局进程预算实例.
static PROCESS: OnceLock<ProcessManager> = OnceLock::new();

/// 初始化全部管理器, 管理器自身不读取全局 getter 或保存彼此.
pub(crate) async fn initialize(config_path: &Path) -> Result<()> {
    let config = register(&CONFIG, AppConfigManager::load(config_path)?, "config")?;
    for directory in [constants::DATA_DIR, constants::DDP_ANI_CACHE_DIR, constants::DDP_DANMAKU_CACHE_DIR] {
        tokio::fs::create_dir_all(directory).await?;
    }
    crate::utils::check_tools().await?;
    register(&DATABASE, DatabaseManager::initialize(Path::new(constants::DATABASE_PATH)).await?, "database")?;
    let processes = register(&PROCESS, ProcessManager::new(config.max_processes, config.max_total_ffmpeg_threads), "process")?;
    register(&CACHE, CacheManager::new(constants::CACHE_DIR.into(), processes).await?, "cache")?;
    register(&SCANNER, ScannerManager::new(Path::new(constants::VIDEO_ROOT)).await?, "scanner")?;
    Ok(())
}

/// 返回唯一应用配置实例.
pub(crate) fn config() -> &'static AppConfigManager {
    CONFIG.get().expect("application config is not initialized")
}

/// 返回唯一数据库实例.
pub(crate) fn database() -> &'static DatabaseManager {
    DATABASE.get().expect("database manager is not initialized")
}

/// 返回唯一扫描实例.
pub(crate) fn scanner() -> &'static ScannerManager {
    SCANNER.get().expect("scanner manager is not initialized")
}

/// 返回唯一缓存实例.
pub(crate) fn cache() -> &'static CacheManager {
    CACHE.get().expect("cache manager is not initialized")
}

/// 返回唯一进程预算实例.
pub(crate) fn process() -> &'static ProcessManager {
    PROCESS.get().expect("process manager is not initialized")
}

/// 注册只读单例, 拒绝覆盖任何已经存在的实例.
fn register<T>(slot: &'static OnceLock<T>, value: T, name: &str) -> Result<&'static T> {
    slot.set(value).map_err(|_| anyhow!("{name} manager is already initialized"))?;
    Ok(slot.get().expect("registered manager must exist"))
}
