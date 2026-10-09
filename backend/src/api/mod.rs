use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde_json::json;

use crate::db::models::ProgressInput;
use crate::{db::repository, AppState};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/files", get(list_files))
        .route("/api/files/{pre16mib_md5}", get(get_file))
        .route(
            "/api/files/{pre16mib_md5}/identify",
            post(identify_file),
        )
        .route(
            "/api/files/{pre16mib_md5}/progress",
            get(get_progress).put(save_progress),
        )
        .route("/api/config", get(get_config))
        .route("/api/scan", post(scan))
        .with_state(state)
}

async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.config.web.clone()))
}

async fn list_files(State(state): State<AppState>) -> impl IntoResponse {
    match repository::list_videos(&state.db).await {
        Ok(items) => (StatusCode::OK, Json(items)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn get_file(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    let video = match repository::get_video(&state.db, &pre16mib_md5).await {
        Ok(Some(video)) => video,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "not found"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let paths = match repository::get_paths(&state.db, &pre16mib_md5).await {
        Ok(paths) => paths,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let media_url = paths
        .first()
        .map(|path| {
            format!(
                "/media/{}",
                path.relative_path()
                    .split('/')
                    .map(url_segment)
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
        .unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!({"video": video, "paths": paths, "media_url": media_url})),
    )
        .into_response()
}

async fn identify_file(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    match repository::set_mock_identification(&state.db, &pre16mib_md5).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "mode": "mock", "ddp_epi_id": 1})),
        )
            .into_response(),
        Err(sqlx::Error::RowNotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not found"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn get_progress(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    match repository::get_progress(&state.db, &pre16mib_md5).await {
        Ok(Some(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not found"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn save_progress(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
    Json(input): Json<ProgressInput>,
) -> impl IntoResponse {
    match repository::save_progress(&state.db, &pre16mib_md5, input.position).await {
        Ok(()) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(sqlx::Error::RowNotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not found"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn scan(State(state): State<AppState>) -> impl IntoResponse {
    match crate::video::scanner::scan_once(&state).await {
        Ok(count) => (StatusCode::OK, Json(json!({"ok": true, "scanned": count}))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn url_segment(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
