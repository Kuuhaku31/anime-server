
// backend/src/api/playback.rs
//! 校验媒体流选择并连接缓存句柄与不透明播放会话.

use axum::{
    body::Body, extract::{rejection::JsonRejection, Path, Request},
    http::{header, HeaderValue, StatusCode}, response::Response,
    routing::{get, post}, Json, Router,
};
use tower_http::services::ServeFile;

use crate::{
    cache::HandleSnapshot, constants::CACHE_VERSION,
    models::{CacheSpec, MediaConfigs, MediaInfo, MediaMetadata, PlaybackQuery, PlaybackStart, StreamInfo},
    services,
};

use super::ApiError;

#[cfg(test)]
#[path = "../tests/api/playback.rs"]
mod tests;

/// 公开创建, 查询, 释放会话和获取媒体资源的路由.
pub(super) fn router() -> Router {
    Router::new()
        .route("/api/playback", post(create_playback))
        .route("/api/playback/{session_id}", get(playback_status).delete(release_playback))
        .route("/playback/{session_id}/{asset_name}", get(playback_asset))
}

/// 重新计算媒体身份并探测元数据, 创建播放后不等待缓存编码.
async fn create_playback(
    input: Result<Json<PlaybackQuery>, JsonRejection>,
) -> Result<Json<PlaybackStart>, ApiError> {
    let Json(query) = input.map_err(ApiError::bad)?;
    services::scanner().check_path(&query.path).map_err(ApiError::bad)?;
    let file = services::scanner().scan_one(services::database(), &query.path).await.map_err(ApiError::media)?
        .ok_or_else(|| ApiError::missing("media file not found"))?;
    let metadata = services::process().probe(&file.path).await.map_err(ApiError::media)?;
    let record = services::database().get_file_by_path(&file.path).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("media path was removed during playback creation"))?;
    if file.media_id != record.media_id {
        return Err(ApiError::media("media path changed during playback creation; retry the request"));
    }
    let spec = select_streams(file.media_id, &query, &metadata, &record.configs)?;
    let cache = services::cache().open(spec.clone(), file.path, metadata.clone(),
        services::config(), services::process()).await.map_err(ApiError::internal)?;
    let session_id = services::sessions().create(cache.folder.clone()).await.map_err(ApiError::internal)?;
    Ok(Json(PlaybackStart {
        url: format!("/playback/{session_id}/index.m3u8"), session_id, cache,
        media: MediaInfo { metadata, configs: record.configs },
        video_stream_index: spec.video_stream_index,
        audio_stream_index: spec.audio_stream_index,
        subtitle_stream_index: spec.subtitle_stream_index,
    }))
}

/// 返回会话映射对应的缓存快照, 不维护目标位置或心跳.
async fn playback_status(Path(session_id): Path<String>) -> Result<Json<HandleSnapshot>, ApiError> {
    let folder = services::sessions().folder(&session_id).await.map_err(ApiError::missing)?;
    services::cache().status(&folder).await.map(Json).map_err(ApiError::internal)
}

/// 只释放映射, 其他会话和持续缓存生成均不受影响.
async fn release_playback(Path(session_id): Path<String>) -> StatusCode {
    services::sessions().release(&session_id).await;
    StatusCode::NO_CONTENT
}

/// 从已发布清单中提供资源, 不允许读取实例描述或私有工作目录.
async fn playback_asset(
    Path((session_id, asset_name)): Path<(String, String)>, request: Request,
) -> Result<Response, ApiError> {
    let folder = services::sessions().folder(&session_id).await.map_err(ApiError::missing)?;
    let path = services::cache().media_path(&folder, &asset_name).await.map_err(ApiError::missing)?;
    let mut response = ServeFile::new(path).try_call(request).await.map_err(ApiError::internal)?.map(Body::new);
    if response.status().is_success() {
        let mime = if asset_name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" }
            else if asset_name.ends_with(".m4s") { "video/iso.segment" } else { "video/mp4" };
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

/// 直接对照元数据校验实际流索引, 不引入额外选轨模型或静默回退.
fn select_streams(
    media_id: String, query: &PlaybackQuery, metadata: &MediaMetadata, configs: &MediaConfigs,
) -> Result<CacheSpec, ApiError> {
    let video_stream_index = query.video_stream_index.or(configs.video_stream_index)
        .or_else(|| metadata.video_streams.first().map(|stream| stream.stream_index))
        .ok_or_else(|| ApiError::media("media has no playable video stream"))?;
    let audio_stream_index = query.audio_stream_index.unwrap_or_else(|| configs.audio_stream_index.or_else(||
        metadata.audio_streams.iter().find(|stream| stream.is_default)
            .or(metadata.audio_streams.first()).map(|stream| stream.stream_index)));
    let subtitle_stream_index = query.subtitle_stream_index.unwrap_or_else(|| configs.subtitle_stream_index.or_else(||
        metadata.subtitle_streams.iter().find(|stream| stream.is_default || stream.is_forced)
            .map(|stream| stream.stream_index)));
    check_stream(&metadata.video_streams, Some(video_stream_index), "video")?;
    check_stream(&metadata.audio_streams, audio_stream_index, "audio")?;
    check_stream(&metadata.subtitle_streams, subtitle_stream_index, "subtitle")?;
    Ok(CacheSpec::new(media_id, video_stream_index, audio_stream_index, subtitle_stream_index, CACHE_VERSION.to_string()))
}

/// 验证索引属于请求的流类别, 显式关闭的可选流不需要查找.
fn check_stream(streams: &[StreamInfo], stream_index: Option<i32>, kind: &str) -> Result<(), ApiError> {
    if stream_index.is_some_and(|index| !streams.iter().any(|stream| stream.stream_index == index)) {
        return Err(ApiError::bad(format!("selected {kind} stream does not exist")));
    }
    Ok(())
}
