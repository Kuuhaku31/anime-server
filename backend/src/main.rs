//! 启动 Web 服务并组装应用依赖.
// backend/src/main.rs

use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::Result;
use axum::{routing::get, Json};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use tokio::net::TcpListener;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::info;

mod api;
mod config;
mod db;
mod tasks;
mod video;

use config::{
    AppConfig, DATABASE_PATH, DATA_DIR, DDP_ANI_CACHE_DIR, DDP_DANMAKU_CACHE_DIR,
    CACHE_DIR,
};
use video::{cache::CacheManager, sessions::SessionManager, metadata::MetadataCache, process::Processes};


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

    println!("Anime Server v{} starting...", env!("CARGO_PKG_VERSION"));

    // 容器探针只检查已有 HTTP 服务, 不重新初始化数据库或争抢缓存独占锁.
    if std::env::args().any(|arg| arg == "--healthcheck") {
        return video::health::http_check();
    }
    tracing_subscriber::fmt()
        .with_env_filter("anime_server=info,tower_http=info")
        .init();

    let config_path =
        std::env::var("ANIME_SERVER_CONFIG").unwrap_or_else(|_| "/config/config.json".to_string());
    let config = AppConfig::load(Path::new(&config_path))?;

    std::fs::create_dir_all(DATA_DIR)?;
    std::fs::create_dir_all(DDP_ANI_CACHE_DIR)?;
    std::fs::create_dir_all(DDP_DANMAKU_CACHE_DIR)?;
    video::health::check_tools().await?;

    let database_options = SqliteConnectOptions::new()
        .filename(DATABASE_PATH)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(database_options)
        .await?;

    sqlx::raw_sql(include_str!("../sql/init.sql"))
        .execute(&pool)
        .await?;

    // 探测, 资源准备, 编码和解码校验共用进程预算, 防止不同模块各自突破全局上限.
    let processes = Processes::new(config.playback.max_processes, config.playback.max_total_ffmpeg_threads);
    let cache = CacheManager::new(CACHE_DIR.into(), config.playback.clone(), processes.clone()).await?;
    let playback = SessionManager::new(cache, config.playback.idle_timeout_seconds);
    let metadata = MetadataCache::new(processes);
    let state = AppState {
        db: pool,
        config: Arc::new(config),
        metadata, playback: playback.clone(),
        scan_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    tasks::scanner::spawn(state.clone());
    state.playback.spawn_expiration();

    // API 和前端静态文件由同一端口提供, 浏览器通过同源相对 URL 访问服务.
    let app = api::router(state)
        .route("/health", get(|| async { Json(serde_json::json!({"status": "ok"})) }))
        .fallback_service(ServeDir::new("/app/web").append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("Anime Server listening on http://{}", addr);
    axum::serve(listener, app).with_graceful_shutdown(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("cannot install SIGTERM handler");
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
    Ok(())
}
