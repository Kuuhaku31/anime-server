
// backend/src/main.rs
//! 启动全局管理器, 后台扫描和同源 HTTP 服务.

use std::{net::SocketAddr, path::Path, time::Duration};

use anyhow::Result;
use axum::{routing::get, Json};
use tokio::{net::TcpListener, signal::unix::SignalKind, sync::watch};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{info, warn};

mod api;
mod cache;
mod config;
mod constants;
mod db;
mod models;
mod process;
mod scanner;
mod services;
mod utils;

#[cfg(test)]
#[path = "../tests/fixtures/runtime.rs"]
mod testing;

/// 初始化唯一实例并在 HTTP 停止后依次回收后台任务和持久资源.
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--healthcheck") { return utils::http_check(); }
    tracing_subscriber::fmt().with_env_filter("anime_server=info,tower_http=info").init();
    let config_path = std::env::var("ANIME_SERVER_CONFIG").unwrap_or_else(|_| "/config/config.json".to_string());
    services::initialize(Path::new(&config_path)).await?;
    let (stop, receiver) = watch::channel(false);
    let scanning = tokio::spawn(scan_periodically(receiver));
    let app = api::router()
        .route("/health", get(health))
        .fallback_service(ServeDir::new("/app/web").append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http());
    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("Anime Server listening on http://{addr}");
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await;
    // 先等待 HTTP 完全停止, 避免清理缓存时仍有请求创建新的写入任务.
    stop.send_replace(true);
    scanning.await?;
    let caches = services::cache().shutdown().await;
    services::database().shutdown().await;
    server?;
    caches?;
    info!("Anime Server exited gracefully");
    Ok(())
}

/// 返回不重新初始化任何管理器的健康状态.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// 周期性同步媒体索引, 关闭时不再启动下一次扫描.
async fn scan_periodically(mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() { return; }
        match services::scanner().scan_all(services::database()).await {
            Ok(changes) => info!(total = changes.total, added = changes.added,
                updated = changes.updated, removed = changes.removed, "media scan completed"),
            Err(error) => warn!("media scan failed: {error:#}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(services::config().scan_interval_seconds)) => {},
            _ = stop.changed() => return,
        }
    }
}

/// 将 SIGTERM 或 Ctrl+C 转为 Axum 的停止接收信号.
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(SignalKind::terminate()).expect("cannot install SIGTERM handler");
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result { tracing::error!("signal handler failed: {error}"); }
        },
        _ = terminate.recv() => {},
    }
}
