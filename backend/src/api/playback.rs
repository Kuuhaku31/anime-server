//! 提供媒体探测和播放会话 API.
// backend/src/api/playback.rs

use std::path::PathBuf;

use axum::{
    body::{Body, Bytes},
    extract::{rejection::JsonRejection, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use serde::Deserialize;
use tower_http::services::ServeFile;
use tracing::warn;

use crate::{
    db::repository,
    video::{ffmpeg::{EncodeSpec, PlaybackRequest}, metadata::MediaInfo, sessions::{PlaybackStart, SessionStatus}},
    AppState,
};


// 构建播放相关 API 路由
pub fn router() -> Router<AppState> {
    Router::new()
    .route("/api/files/{id}/streams", get(streams))
    .route("/api/files/{id}/probe", post(probe))
    .route("/api/files/{id}/playback", post(play))
    .route("/api/playback/{id}/status", get(status))
    .route("/api/playback/{id}/heartbeat", post(heartbeat))
    .route("/api/playback/{id}/target", post(target))
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

// 1. 根据文件 ID 找到源视频
// 2. 从数据库读取进度, 校验可选音轨和字幕选择
// 3. 由服务端构造播放配置, 客户端只获得不透明会话
async fn play(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Bytes
) -> Result<Json<PlaybackStart>, ApiError> {
    // 空请求体使用服务端默认值, JSON 中显式 null 则保留为关闭轨道的意图.
    let input = if input.is_empty() { PlaybackRequest::default() }
        else { serde_json::from_slice(&input).map_err(ApiError::bad)? };
    let (source, info) = metadata(&state, &id, false).await?;
    let progress = repository::get_progress(&state.db, &id).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("file not found"))?;
    let spec = EncodeSpec::new(&id, source, info, input, progress.watch_pos).map_err(ApiError::bad)?;
    Ok(Json(state.playback.create(spec).await.map_err(ApiError::internal)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetRequest { position: f64 }

// 此接口只更新会话等待目标, 观看记录仍由独立的 progress 接口保存.
async fn target(State(state): State<AppState>, Path(id): Path<String>,
    input: Result<Json<TargetRequest>, JsonRejection>) -> Result<Json<SessionStatus>, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;
    let status = state.playback.status(&id).await.map_err(ApiError::missing)?;
    if !input.position.is_finite() || input.position < 0.0 || input.position >= status.duration {
        return Err(ApiError::bad("position must be within the video duration"));
    }
    Ok(Json(state.playback.target(&id, input.position).await.map_err(ApiError::missing)?))
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
    // _lease 持有到 ServeFile 完成文件打开, 防止最后释放触发的 LRU 抢先删除目录.
    let (path, mime, _lease) = state.playback.media_path(&id, &asset).await.map_err(ApiError::missing)?;
    let response = ServeFile::new(path).try_call(request).await.map_err(ApiError::internal)?;
    let mut response = response.map(Body::new);
    if response.status().is_success() {
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    }
    // event 清单会增长且会话会过期, 禁止 HTTP 缓存掩盖新分片或失效状态.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    Ok(response)
}
