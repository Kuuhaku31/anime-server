//! 定义媒体探测, 播放会话及播放资源 API.

use std::path::PathBuf;

use axum::{
    body::{Body, Bytes},
    extract::{rejection::JsonRejection, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::ServeFile;
use tracing::warn;

use crate::{
    AppState, constants::VIDEO_ROOT, db::repository, utils, video::{
        ffmpeg::{EncodeSpec, PlaybackRequest},
        metadata::MediaInfo,
        sessions::{PlaybackStart, SessionStatus},
    },
};


/// 构建播放相关 API 路由.
pub(super) fn router() -> Router<crate::AppState> {
    Router::new()
    .route("/api/files/{id}/streams", get(streams))
    .route("/api/files/{id}/probe", post(probe))
    .route("/api/files/{id}/playback", post(play))
    .route("/api/playback/{id}/status", get(status))
    .route("/api/playback/{id}/heartbeat", post(heartbeat))
    .route("/api/playback/{id}/target", post(target))
    .route("/api/playback/{id}", delete(release))
    .route("/playback/{id}/{asset}", get(media))
}


// ========================================================================== //

/// 播放 API 错误.
struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::BAD_REQUEST, error.to_string())
    }

    fn media(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }

    fn missing(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::NOT_FOUND, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        warn!(
            status = self.0.as_u16(),
            error = %self.1,
            "playback API error"
        );

        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}



/// 获取已缓存的媒体流信息.
async fn streams(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<MediaInfo>, ApiError> {
    Ok(Json(metadata(&state, &id, false).await?.1))
}

/// 重新探测媒体文件.
async fn probe(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<MediaInfo>, ApiError> {
    Ok(Json(metadata(&state, &id, true).await?.1))
}

/// 创建播放会话.
///
/// 客户端只提交播放选项, 播放配置由服务端生成.
///
/// 定义了 POST /api/files/{id}/playback 的处理函数:
/// 根据客户端请求和服务器数据创建播放会话, 最后返回 JSON 格式的会话信息.
async fn play(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Bytes,
) -> Result<Json<PlaybackStart>, ApiError>
{
    // 空请求体使用默认配置, 显式 null 仍交由 JSON 反序列化处理.
    let input = if input.is_empty() { PlaybackRequest::default() }
    else { serde_json::from_slice(&input).map_err(ApiError::bad)? };

    let (source, info) = metadata(&state, &id, false).await?;

    let progress = repository::get_progress(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("file not found"))?;

    // 请求显式选择覆盖数据库偏好, 字段缺省才使用扫描或上次手动选择保存的轨道.
    let audio_preference = input.audio_stream_index;
    let subtitle_preference = input.subtitle_stream_index;
    let request = input.with_preferences(progress.audio_stream_index, progress.subtitle_stream_index);
    let spec = EncodeSpec::new(&id, source, info, request, progress.watch_pos).map_err(ApiError::bad)?;

    // 只保存本次请求显式提交的选择, null 也表示将对应偏好设为关闭.
    repository::save_track_preferences(
        &state.db, &id, audio_preference, subtitle_preference,
    ).await.map_err(ApiError::internal)?;

    let session = state
        .playback
        .create(spec)
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(session))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetRequest {
    position: f64,
}

/// 更新播放会话的目标位置, 不保存观看进度.
async fn target(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Result<Json<TargetRequest>, JsonRejection>,
) -> Result<Json<SessionStatus>, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;

    let status = state
        .playback
        .status(&id)
        .await
        .map_err(ApiError::missing)?;

    if !input.position.is_finite() || input.position < 0.0 || input.position >= status.duration {
        return Err(ApiError::bad("position must be within the video duration"));
    }

    let status = state
        .playback
        .target(&id, input.position)
        .await
        .map_err(ApiError::missing)?;

    Ok(Json(status))
}

/// 获取播放会话状态.
async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionStatus>, ApiError> {
    Ok(Json(
        state
            .playback
            .status(&id)
            .await
            .map_err(ApiError::missing)?,
    ))
}

/// 刷新播放会话心跳.
async fn heartbeat(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .playback
        .heartbeat(&id)
        .await
        .map_err(ApiError::missing)?;

    Ok(StatusCode::NO_CONTENT)
}

/// 释放播放会话.
async fn release(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .playback
        .release(&id)
        .await
        .map_err(ApiError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

/// 返回播放会话中的媒体资源.
async fn media(
    State(state): State<AppState>,
    Path((id, asset)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    // 持有 lease 直到 ServeFile 完成文件打开, 避免资源目录被提前删除.
    let (path, mime, _lease) = state
        .playback
        .media_path(&id, &asset)
        .await
        .map_err(ApiError::missing)?;

    let response = ServeFile::new(path)
        .try_call(request)
        .await
        .map_err(ApiError::internal)?;

    let mut response = response.map(Body::new);

    if response.status().is_success() {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    }

    // 防止事件清单和分片的缓存掩盖会话状态变化.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    Ok(response)
}


/// 获取媒体文件路径和元数据.
async fn metadata(
    state: &AppState,
    id: &str,
    retry: bool,
) -> Result<(std::path::PathBuf, MediaInfo), ApiError> {

    /// 根据数据库路径查找可用媒体文件副本.
    ///
    /// 规范化路径并检查符号链接目标, 确保最终文件仍位于媒体根目录内.
    async fn media_path(state: &AppState, id: &str) -> Result<PathBuf, String> {

        let id = utils::check_md5(id)?;

        let root = tokio::fs::canonicalize(VIDEO_ROOT)
        .await.map_err(|e| e.to_string())?;
        let paths = repository::get_video_paths_by_id(&state.db, &id)
        .await.map_err(|e| e.to_string())?;

        // 遍历路径
        for f in paths {

            let resolved = match tokio::fs::canonicalize(&f).await {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };

            if !resolved.starts_with(&root) {
                return Err("media path resolves outside media directory".to_string());
            }

            if tokio::fs::metadata(&resolved)
                .await
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Ok(resolved);
            }
        }

        Err("no available media path for file".to_string())
    }

    let path = media_path(state, id).await.map_err(ApiError::missing)?;

    let info = state
        .metadata
        .get(id, &path, retry)
        .await
        .map_err(ApiError::media)?;

    Ok((path, info))
}
