
// backend/src/main.rs
//! 启动 Web 服务并组装应用依赖.

use std::{net::SocketAddr, path::Path, sync::Arc};
use tracing::error;

use anyhow::Result;
use axum::{routing::get, Json};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use tokio::{net::TcpListener, signal::unix::SignalKind};
use tower_http::{services::ServeDir, trace::TraceLayer};

mod api;
mod config;
mod db;
mod video;
mod utils;
mod constants;
mod models;

use config::AppConfig;
use video::{cache::CacheManager, sessions::SessionManager, metadata::MetadataCache, process::Processes};

use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use crate::{models::ScanChanges, video::scanner};
use crate::db::repository as db_repository;

#[derive(Clone)]
/// 应用状态
pub struct AppState {
    pub db: SqlitePool, // 数据库连接池
    pub config: Arc<AppConfig>, // 应用配置
    pub metadata: MetadataCache, // 元数据缓存
    pub playback: SessionManager, // 播放会话管理
    pub scan_lock: Arc<tokio::sync::Mutex<()>>, // 扫描锁, 防止同时进行多个扫描任务
}


#[tokio::main]
async fn main() -> Result<()> {

    println!("Anime Server v{} Starting...", env!("CARGO_PKG_VERSION"));

    // 容器探针只检查已有 HTTP 服务, 不重新初始化数据库或争抢缓存独占锁.
    if std::env::args().any(|arg| arg == "--healthcheck") { return utils::http_check(); }
    // 初始化 Rust 程序的日志系统, 设置日志过滤规则, 并将日志输出到终端
    tracing_subscriber::fmt().with_env_filter("anime_server=info,tower_http=info").init();

    // 加载配置
    let config_path = std::env::var("ANIME_SERVER_CONFIG").unwrap_or_else(|_| "/config/config.json".to_string());
    let config = AppConfig::load(Path::new(&config_path))?;
    info!("loaded config from {}", config_path);

    // 创建数据目录和缓存目录, 并检查 FFmpeg 等工具是否可用
    std::fs::create_dir_all(constants::DATA_DIR)?;
    std::fs::create_dir_all(constants::DDP_ANI_CACHE_DIR)?;
    std::fs::create_dir_all(constants::DDP_DANMAKU_CACHE_DIR)?;
    match utils::check_tools().await {
        Ok(_) => (),
        Err(e) => {
            error!("tools check failed: {:#}", e);
            error!("ffmpeg and ffprobe are required for media processing, please install them and ensure they are in the PATH");
        },
    }
    info!("tools check passed, ffmpeg and ffprobe are available");

    // 初始化数据库连接池, 并执行初始化 SQL 脚本, 创建必要的表和索引
    let database_options = SqliteConnectOptions::new()
    .filename(constants::DATABASE_PATH)
    .create_if_missing(true)
    .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
    .max_connections(5)
    .connect_with(database_options)
    .await?;
    sqlx::raw_sql(include_str!("../sql/init.sql"))
    .execute(&pool)
    .await?;
    info!("database initialized at {}", constants::DATABASE_PATH);

    // 探测, 资源准备, 编码和解码校验共用进程预算, 防止不同模块各自突破全局上限.
    let processes = Processes::new(config.playback.max_processes, config.playback.max_total_ffmpeg_threads);
    let cache = CacheManager::new(constants::CACHE_DIR.into(), config.playback.clone(), processes.clone()).await?;
    info!("cache manager initialized at {}", constants::CACHE_DIR);
    let playback = SessionManager::new(cache, config.playback.idle_timeout_seconds);
    let metadata = MetadataCache::new(processes);
    let state = AppState {
        db: pool,
        config: Arc::new(config),
        metadata, playback: playback.clone(),
        scan_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    // 启动后台扫描任务, 定期扫描视频目录并更新数据库和元数据缓存.
    let s = state.clone();
    tokio::spawn(async move {
        loop {
            match scan_once(&s).await {
                Ok(ScanChanges { added, removed, count }) => 
                info!("video scan completed: {} files, {} added, {} removed", 
                count, added.len(), removed.len()),
                Err(e) => warn!("video scan failed: {}", e),
            }
            sleep(Duration::from_secs(s.config.scan_interval_seconds)).await;
        }
    });
    state.playback.spawn_expiration(); // 启动一个后台异步任务, 定期回收失去联系的播放会话并维护磁盘缓存
    info!("background tasks started: video scan and playback session expiration");

    // API 和前端静态文件由同一端口提供, 浏览器通过同源相对 URL 访问服务.
    let app = api::router(state)
    .route("/health", get(|| async { Json(serde_json::json!({"status": "ok"})) }))

    // 前端静态文件服务: /app/web
    .fallback_service(ServeDir::new("/app/web").append_index_html_on_directories(true))
    .layer(TraceLayer::new_for_http());
    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Anime Server listening on http://{}", addr);

    // 启动 HTTP 服务, 并在收到 SIGTERM 或 Ctrl+C 信号时优雅关闭, 等待子进程退出.
    axum::serve(listener, app).with_graceful_shutdown(async move {

        // 安装 SIGTERM 信号处理器
        let mut terminate = tokio::signal::unix::signal(SignalKind::terminate()).expect("cannot install SIGTERM handler");

        // 等待 SIGTERM 或 Ctrl+C 信号
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result { tracing::error!("signal handler failed: {error}"); }
            },
            _ = terminate.recv() => {},
        }

        // 等待分片边界落盘并回收子进程, 不依赖容器强制终止来结束转码.
        if let Err(error) = playback.shutdown().await {
            tracing::error!("playback shutdown failed: {error:#}");
        }
    }).await?;

    info!("Anime Server exited gracefully");

    Ok(())
}


pub(crate) async fn scan_once(state: &AppState) -> Result<ScanChanges, String> {

    // 获取数据库中已知的路径列表
    let paths_in_db = db::repository::get_paths(&state.db).await
    .map_err(|e| format!("failed to read paths from database: {e}"))?;

    // 据此扫描视频目录, 返回新增和删除的文件列表
    let changes = scanner::scan_once(&state.scan_lock, paths_in_db).await?;

    // 写入失败必须返回扫描错误, 避免旧路径未更新却向客户端报告成功.
    db_repository::update(&state.db, &changes).await
        .map_err(|e| format!("failed to update scanned files in database: {e}"))?;

    info!("scan_once: {} files scanned, {} added, {} removed", changes.count, changes.added.len(), changes.removed.len());

    Ok(changes)
}