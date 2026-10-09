use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{rejection::JsonRejection, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tower_http::services::ServeFile;
use tracing::warn;

use crate::{
    video::{ffmpeg::{EncodeSpec, PlaybackRequest}, metadata::MediaInfo, sessions::SessionStatus},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/files/{id}/streams", get(streams))
        .route("/api/files/{id}/probe", post(probe))
        .route("/api/files/{id}/playback-plan", post(plan))
        .route("/api/playback/{id}/status", get(status))
        .route("/api/playback/{id}/heartbeat", post(heartbeat))
        .route("/api/playback/{id}", axum::routing::delete(release))
        .route("/playback/{id}/{asset}", get(media))
}

struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(error: impl std::fmt::Display) -> Self { Self(StatusCode::BAD_REQUEST, error.to_string()) }
    fn media(error: impl std::fmt::Display) -> Self { Self(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()) }
    fn internal(error: impl std::fmt::Display) -> Self { Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()) }
    fn missing(error: impl std::fmt::Display) -> Self { Self(StatusCode::NOT_FOUND, error.to_string()) }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        warn!(status = self.0.as_u16(), "playback API error: {}", self.1);
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

async fn metadata(state: &AppState, id: &str, retry: bool) -> Result<(PathBuf, MediaInfo), ApiError> {
    let path = super::media_path(state, id).await.map_err(ApiError::missing)?;
    let info = state.metadata.get(id, &path, retry).await.map_err(ApiError::media)?;
    Ok((path, info))
}

async fn streams(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<MediaInfo>, ApiError> {
    Ok(Json(metadata(&state, &id, false).await?.1))
}

async fn probe(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<MediaInfo>, ApiError> {
    Ok(Json(metadata(&state, &id, true).await?.1))
}

async fn plan(State(state): State<AppState>, Path(id): Path<String>,
    input: Result<Json<PlaybackRequest>, JsonRejection>) -> Result<Json<SessionStatus>, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;
    let (source, info) = metadata(&state, &id, false).await?;
    let spec = EncodeSpec::new(source, info, input, state.config.playback.ffmpeg_threads).map_err(ApiError::bad)?;
    Ok(Json(state.playback.create(spec).await.map_err(ApiError::internal)?))
}

async fn status(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<SessionStatus>, ApiError> {
    Ok(Json(state.playback.status(&id).await.map_err(ApiError::missing)?))
}

async fn heartbeat(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    state.playback.heartbeat(&id).await.map_err(ApiError::missing)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn release(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    state.playback.release(&id).await.map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn media(State(state): State<AppState>, Path((id, asset)): Path<(String, String)>, request: Request)
    -> Result<Response, ApiError> {
    let (path, mime) = state.playback.media_path(&id, &asset).await.map_err(ApiError::missing)?;
    let response = ServeFile::new(path).try_call(request).await.map_err(ApiError::internal)?;
    let mut response = response.map(Body::new);
    if response.status().is_success() {
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    Ok(response)
}
