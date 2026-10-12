//! 启动 Web 服务并组装应用依赖.

use std::{net::SocketAddr, path::Path, sync::Arc};
use tracing::error;

use anyhow::Result;
use axum::{routing::get, Json};
use sqlx::SqlitePool;
use tokio::{net::TcpListener, signal::unix::SignalKind};
use tower_http::{services::ServeDir, trace::TraceLayer};

mod api;
mod config;
mod db;
mod video;
mod utils;
mod constants;
mod models;
mod scanner;

use config::AppConfig;
use video::{CacheManager, SessionManager, MetadataCache, Processes};

use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use crate::models::ScanChanges;

#[derive(Clone)]
/// 应用状态
struct AppState {
    db: SqlitePool, // 根模块私有字段仍可供其子模块访问.
    config: Arc<AppConfig>, // 应用配置
    metadata: MetadataCache, // 元数据缓存
    playback: SessionManager, // 播放会话管理
    scan_lock: Arc<tokio::sync::Mutex<()>>, // 扫描锁, 防止同时进行多个扫描任务
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


    // 探测, 资源准备, 编码和解码校验共用进程预算, 防止不同模块各自突破全局上限.
    let processes = Processes::new(config.playback.max_processes, config.playback.max_total_ffmpeg_threads);
    let cache = CacheManager::new(constants::CACHE_DIR.into(), config.playback.clone(), processes.clone()).await?;

    info!("cache manager initialized at {}", constants::CACHE_DIR);

    let playback = SessionManager::new(cache, config.playback.idle_timeout_seconds);
    let metadata = MetadataCache::new(processes);

    let state = AppState {
        db: db::initialize_database().await?,
        config: Arc::new(config),
        metadata, playback: playback.clone(),
        scan_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    // 启动后台扫描任务, 定期扫描视频目录并更新数据库和元数据缓存.
    let s = state.clone();
    tokio::spawn(async move {
        loop {
            match scanner::scan_root(&s).await {
                Ok(ScanChanges { added, removed, total }) => 
                info!("video scan completed: {} files, {} added, {} removed", 
                total, added, removed),
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
