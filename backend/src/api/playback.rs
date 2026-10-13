
//! 提供媒体探测、播放会话和播放资源的 HTTP API.

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
    AppState, db, models::{self, MediaInfo}, video,
};


/// 构建播放相关 API 路由.
pub(super) fn router() -> Router<crate::AppState> {
    Router::new()
    .route("/api/files/{file_id}/streams", get(streams))
    .route("/api/files/{file_id}/probe", post(probe))
    .route("/api/files/{file_id}/playback", post(play))
    .route("/api/playback/{session_id}/status", get(status))
    .route("/api/playback/{session_id}/heartbeat", post(heartbeat))
    .route("/api/playback/{session_id}/target", post(target))
    .route("/api/playback/{session_id}", delete(release))
    .route("/playback/{session_id}/{asset_name}", get(media))
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
async fn streams(State(s): State<AppState>, Path(file_id): Path<String>)
    -> Result<Json<MediaInfo>, ApiError>
{
    let cache = s.cache_manager;
    return match cache.media_info(&file_id).await {
        Ok (media_info) => Ok (Json(media_info)),
        Err(error) => Err(if error.to_string() == "video file not found" { ApiError::missing(error) }
            else { ApiError::media(error) }),
    };
}

/// 重新探测媒体文件.
async fn probe(State(s): State<AppState>, Path(file_id): Path<String>)
    -> Result<Json<MediaInfo>, ApiError>
{
    let cache = s.cache_manager;
    return match cache.media_info_force(&file_id).await {
        Ok(media_info) => Ok(Json(media_info)),
        Err(error) => Err(if error.to_string() == "video file not found" { ApiError::missing(error) }
            else { ApiError::media(error) }),
    };
}

/// 创建播放会话.
///
/// 客户端只提交播放选项, 播放配置由服务端生成.
///
/// 定义了 POST /api/files/{file_id}/playback 的处理函数:
/// 根据客户端请求和服务器数据创建播放会话, 最后返回 JSON 格式的会话信息.
async fn play(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
    input: Bytes,
) -> Result<Json<models::PlaybackStart>, ApiError>
{
    // 空请求体使用默认配置, 显式 null 仍交由 JSON 反序列化处理.
    let input = if input.is_empty() { models::PlaybackRequest::default() }
    else { serde_json::from_slice(&input).map_err(ApiError::bad)? };

    // 读取数据库中保存的播放进度
    let progress = db::get_progress(&state.db, &file_id).await
    .map_err(ApiError::internal)?.ok_or_else(|| ApiError::missing("file not found"))?;

    // 请求显式选择覆盖数据库偏好, 字段缺省才使用扫描或上次手动选择保存的轨道.
    let audio_preference = input.audio_stream_index;
    let subtitle_preference = input.subtitle_stream_index;
    let request = input.with_preferences(progress.audio_stream_index, progress.subtitle_stream_index)
        .with_saved_position(progress.watch_pos);

    // 只保存本次请求显式提交的选择, null 也表示将对应偏好设为关闭.
    db::save_track_preferences(&state.db, &file_id, audio_preference, subtitle_preference)
    .await.map_err(ApiError::internal)?;

    // 创建播放会话并返回结果.
    let session = state.session_manager.create(&state.cache_manager, &file_id, request)
        .await.map_err(ApiError::internal)?;

    Ok(Json(session))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetRequest {
    position: f64,
}

/// 更新播放会话的目标位置, 不保存观看进度.
async fn target(
    State(s) : State<AppState>,
    Path (session_id) : Path<String>,
    input    : Result<Json<TargetRequest>, JsonRejection>
) -> Result<Json<video::SessionStatus>, ApiError>
{

    let session     = s.session_manager;
    let cache       = s.cache_manager;
    let Json(input) = input.map_err(ApiError::bad)?;

    let session_status = session.status(&cache, &session_id).await.map_err(ApiError::missing)?;

    // 检查目标位置是否合法.
    if !input.position.is_finite() || input.position < 0.0 || input.position >= session_status.duration {
        return Err(ApiError::bad("position must be within the video duration"));
    }

    return match session.target(&cache, &session_id, input.position).await {
        Ok(session_status) => Ok(Json(session_status)),
        Err(error) => Err(ApiError::missing(error)),
    };
}

/// 获取播放会话状态.
async fn status(State(s): State<AppState>, Path(session_id): Path<String>)
    -> Result<Json<video::SessionStatus>, ApiError>
{
    let session = s.session_manager;
    let cache   = s.cache_manager;
    return match session.status(&cache, &session_id).await {
        Ok (session_status) => Ok (Json(session_status)),
        Err(error) => Err(ApiError::missing(error)),
    };
}

/// 刷新播放会话心跳.
async fn heartbeat(State(s): State<AppState>, Path(session_id): Path<String>)
    -> Result<StatusCode, ApiError>
{
    let session = s.session_manager;
    let cache   = s.cache_manager;
    return match session.heartbeat(&cache, &session_id).await {
        Ok (_) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(ApiError::missing(error)),
    };
}

/// 释放播放会话.
async fn release(State(s): State<AppState>, Path(session_id): Path<String>)
    -> Result<StatusCode, ApiError>
{
    let session = s.session_manager;
    let cache   = s.cache_manager;
    return match session.release(&cache, &session_id).await {
        Ok (_) => Ok (StatusCode::NO_CONTENT),
        Err(error) => Err(ApiError::missing(error)),
    };
}

/// 返回播放会话中的媒体资源.
async fn media(
    State(s): State<AppState>,
    Path((session_id, asset_name)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {

    let session = s.session_manager;
    let cache   = s.cache_manager;

    // 会话只返回已验证的路径, 文件由 ServeFile 打开.
    let path = session
    .media_path(&cache, &session_id, &asset_name).await.map_err(ApiError::missing)?;

    let response = ServeFile::new(path)
    .try_call(request).await.map_err(ApiError::internal)?;

    let mut response = response.map(Body::new);

    if response.status().is_success() {
        // HTTP 层统一设置媒体类型, 保持清单和分片的响应头不变.
        let mime =
        if asset_name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" }
        else if asset_name.ends_with(".m4s") { "video/iso.segment" }
        else { "video/mp4" };
        response.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    }

    // 防止事件清单和分片的缓存掩盖会话状态变化.
    response.headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    Ok(response)
}
