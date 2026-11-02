
// backend/src/api.rs
//! 聚合全局 Manager 并公开按媒体路径操作的 HTTP 契约.

use std::path::PathBuf;

use axum::{
    extract::rejection::JsonRejection, http::StatusCode,
    response::{IntoResponse, Response}, routing::{get, post, put}, Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{models::{MediaConfigs, MediaInfo, ScanChanges}, services};

mod playback;
mod sessions;

pub(crate) use sessions::SessionManager;

/// 组装业务路由, 不向 Axum 注入业务 AppState.
pub(crate) fn router() -> Router {
    Router::new()
        .route("/api/config", get(get_config))
        .route("/api/media", get(list_media))
        .route("/api/media/scan", post(scan_media))
        .route("/api/media/rescan", post(rescan_media))
        .route("/api/media/configs", put(save_media_configs))
        .merge(playback::router())
}

/// 提供前端初始化主题所需的公开配置.
async fn get_config() -> Json<serde_json::Value> {
    Json(json!({"default_dark_mode": services::config().default_dark_mode}))
}

/// 逐路径探测元数据并聚合数据库配置, 任一失败都显式返回错误.
async fn list_media() -> Result<Json<Vec<(PathBuf, MediaInfo)>>, ApiError> {
    let records = services::database().list_paths_with_configs().await.map_err(ApiError::internal)?;
    let mut media = Vec::with_capacity(records.len());
    for record in records {
        let path = record.0;
        let configs = record.1;
        let source_path = services::scanner().resolve(&path).await.map_err(ApiError::media)?;
        let metadata = services::process().probe(&source_path).await.map_err(ApiError::media)?;
        media.push((path, MediaInfo { metadata, configs }));
    }
    Ok(Json(media))
}

/// 请求扫描器同步全部媒体路径.
async fn scan_media() -> Result<Json<ScanChanges>, ApiError> {
    services::scanner().scan_all(services::database()).await.map(Json).map_err(ApiError::internal)
}

/// 定位需要重扫的单个媒体路径.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaPathInput {
    /// 容器内的媒体绝对路径.
    path: PathBuf,
}

/// 重扫单路径并返回最新媒体条目, 文件已删除时返回 null.
async fn rescan_media(
    input: Result<Json<MediaPathInput>, JsonRejection>,
) -> Result<Json<Option<(PathBuf, MediaInfo)>>, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;
    services::scanner().check_path(&input.path).map_err(ApiError::bad)?;
    let Some(file) = services::scanner().scan_one(services::database(), &input.path).await.map_err(ApiError::media)?
        else { return Ok(Json(None)); };
    let metadata = services::process().probe(&file.path).await.map_err(ApiError::media)?;
    let record = services::database().get_file_by_path(&file.path).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("media path was removed during rescan"))?;
    Ok(Json(Some((file.path, MediaInfo { metadata, configs: record.1 }))))
}

/// 按路径替换完整的共享播放配置.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveMediaConfigsInput {
    /// 容器内媒体路径.
    path: PathBuf,
    /// 要保存的进度和全部流偏好.
    configs: MediaConfigs,
}

/// 校验配置形状后保存, 不把前端的等待目标当作播放进度.
async fn save_media_configs(
    input: Result<Json<SaveMediaConfigsInput>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;
    services::scanner().check_path(&input.path).map_err(ApiError::bad)?;
    if input.configs.position.is_some_and(|position| !position.is_finite() || position < 0.0) {
        return Err(ApiError::bad("position must be finite and non-negative"));
    }
    if [input.configs.video_stream_index, input.configs.audio_stream_index, input.configs.subtitle_stream_index]
        .into_iter().flatten().any(|index| index < 0) {
        return Err(ApiError::bad("stream indices must be non-negative"));
    }
    services::database().save_configs_by_path(&input.path, &input.configs).await.map_err(|error| {
        if matches!(error, sqlx::Error::RowNotFound) { ApiError::missing(error) }
        else { ApiError::internal(error) }
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// 保存业务错误状态和可记录的诊断文本.
#[derive(Debug)]
struct ApiError {
    /// HTTP 响应状态.
    status: StatusCode,
    /// 返回并记录的错误说明.
    message: String,
}

impl ApiError {
    /// 表示客户端请求格式或选择不合法.
    fn bad(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: error.to_string() }
    }

    /// 表示源媒体无法探测或读取.
    fn media(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::UNPROCESSABLE_ENTITY, message: error.to_string() }
    }

    /// 表示内部 I/O 或管理器操作失败.
    fn internal(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: error.to_string() }
    }

    /// 表示媒体, 会话或资源不存在.
    fn missing(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: error.to_string() }
    }
}

impl IntoResponse for ApiError {
    /// 统一记录并序列化 API 错误, 不提供成功形状的默认数据.
    fn into_response(self) -> Response {
        tracing::warn!(status = self.status.as_u16(), error = self.message, "API request failed");
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}
