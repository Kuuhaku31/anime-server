use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::Result;
use axum::{http::StatusCode, response::IntoResponse, routing::get};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::info;

mod api;
mod config;
mod db;
mod tasks;
mod video;

use config::{
    AppConfig, DATABASE_PATH, DATA_DIR, DDP_ANI_CACHE_DIR, DDP_DANMAKU_CACHE_DIR, VIDEO_ROOT,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<AppConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("anime_server=info,tower_http=info")
        .init();

    let config_path =
        std::env::var("ANIME_SERVER_CONFIG").unwrap_or_else(|_| "/config/config.json".to_string());
    let config = AppConfig::load(Path::new(&config_path))?;

    std::fs::create_dir_all(DATA_DIR)?;
    std::fs::create_dir_all(DDP_ANI_CACHE_DIR)?;
    std::fs::create_dir_all(DDP_DANMAKU_CACHE_DIR)?;

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

    let state = AppState {
        db: pool,
        config: Arc::new(config),
    };

    tasks::scanner::spawn(state.clone());

    let app = api::router(state)
        .nest_service("/media", ServeDir::new(VIDEO_ROOT))
        .route("/health", get(health))
        .fallback_service(ServeDir::new("/app/web").append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("Anime Server listening on http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}
