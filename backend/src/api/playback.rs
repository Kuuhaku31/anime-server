
// backend/src/api/playback.rs
//! 分离缓存构建管理与按稳定句柄读取 HLS 资源的请求.

use std::path::{Path as FilePath, PathBuf};

use axum::{
    body::Body, extract::{rejection::JsonRejection, Path, Request},
    http::{header, HeaderValue, StatusCode}, response::Response,
    routing::{get, post}, Json, Router,
};
use tower_http::services::ServeFile;

use crate::{
    cache::{check_handle_id, CacheSnapshot}, constants::CACHE_VERSION, db::DatabaseManager,
    models::{CacheBuild, CacheEntry, CacheList, CacheSpec, MediaConfigs, MediaInfo, MediaMetadata, PlaybackQuery, StreamInfo},
    process::{self, ProcessManager, WorkerResources}, scanner::ScannerManager, services,
};

use super::ApiError;

#[cfg(test)]
#[path = "../tests/api/playback.rs"]
mod tests;

/// 构建和管理缓存使用 API, 读取播放资源不创建服务端会话.
pub(super) fn router() -> Router {
    Router::new()
        .route("/api/cache", get(list_cache).post(build_cache))
        .route("/api/cache/{handle_id}", get(cache_detail).delete(delete_cache))
        .route("/api/cache/{handle_id}/retry", post(retry_cache))
        .route("/api/cache/{handle_id}/stop", post(stop_cache))
        .route("/playback/{handle_id}/{asset_name}", get(playback_asset))
}

/// 接受 PlaybackQuery 并启动或复用转码, 不创建播放映射或等待编码完成.
async fn build_cache(
    input: Result<Json<PlaybackQuery>, JsonRejection>,
) -> Result<Json<CacheBuild>, ApiError> {
    let Json(query) = input.map_err(ApiError::bad)?;
    let (media_id, media) = prepare_media(&query.path, None, services::database(),
        services::scanner(), services::process()).await?;
    let spec = select_streams(media_id, &query, &media.metadata, &media.configs)?;
    let snapshot = services::cache().build(spec, query.path, media.metadata.clone(),
        services::config(), services::process()).await.map_err(ApiError::cache)?;
    Ok(Json(CacheBuild { cache: cache_entry(snapshot).await?, media }))
}

/// 获取全部缓存及转码 worker 占用合计, 不探测媒体或启动任务.
async fn list_cache() -> Result<Json<CacheList>, ApiError> {
    let snapshots = services::cache().list().await.map_err(ApiError::cache)?;
    let mut entries = Vec::with_capacity(snapshots.len());
    let mut resources = WorkerResources::default();
    let mut sources: std::collections::HashMap<String, Option<PathBuf>> = std::collections::HashMap::new();
    for snapshot in snapshots {
        resources.encoder_slots += snapshot.status.resources.encoder_slots;
        resources.process_slots += snapshot.status.resources.process_slots;
        resources.ffmpeg_threads += snapshot.status.resources.ffmpeg_threads;
        let source_path = if let Some(path) = sources.get(&snapshot.spec.media_id) { path.clone() }
            else {
                let path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;
                sources.insert(snapshot.spec.media_id.clone(), path.clone());
                path
            };
        entries.push(CacheEntry { snapshot, source_path });
    }
    Ok(Json(CacheList { entries, resources }))
}

/// 查询单个句柄, 不把查询请求变成构建请求.
async fn cache_detail(Path(handle_id): Path<String>) -> Result<Json<CacheEntry>, ApiError> {
    check_handle_id(&handle_id).map_err(ApiError::bad)?;
    let snapshot = services::cache().status(&handle_id).await.map_err(ApiError::cache)?;
    cache_entry(snapshot).await.map(Json)
}

/// 重新校验源文件身份和全部实际流, 仅重试已有句柄.
async fn retry_cache(Path(handle_id): Path<String>) -> Result<Json<CacheBuild>, ApiError> {
    check_handle_id(&handle_id).map_err(ApiError::bad)?;
    let spec = services::cache().spec(&handle_id).await.map_err(ApiError::cache)?;
    let source_path = source_path(&spec.media_id, services::database(), services::scanner()).await?
        .ok_or_else(|| ApiError::missing("cache source media is unavailable"))?;
    let (_, media) = prepare_media(&source_path, Some(&spec.media_id), services::database(),
        services::scanner(), services::process()).await?;
    check_stream(&media.metadata.video_streams, Some(spec.video_stream_index), "video")?;
    check_stream(&media.metadata.audio_streams, spec.audio_stream_index, "audio")?;
    check_stream(&media.metadata.subtitle_streams, spec.subtitle_stream_index, "subtitle")?;
    let snapshot = services::cache().retry(&handle_id, source_path, media.metadata.clone(),
        services::config(), services::process()).await.map_err(ApiError::cache)?;
    Ok(Json(CacheBuild { cache: cache_entry(snapshot).await?, media }))
}

/// 即使客户端断开也完成停止和资源回收, 不删除已发布前缀.
async fn stop_cache(Path(handle_id): Path<String>) -> Result<Json<CacheEntry>, ApiError> {
    check_handle_id(&handle_id).map_err(ApiError::bad)?;
    let snapshot = tokio::spawn(async move { services::cache().stop(&handle_id).await })
        .await.map_err(ApiError::internal)?.map_err(ApiError::cache)?;
    cache_entry(snapshot).await.map(Json)
}

/// 删除由后台入口完整执行, 不因 HTTP 请求取消而留下封闭句柄和半删除目录.
async fn delete_cache(Path(handle_id): Path<String>) -> Result<StatusCode, ApiError> {
    check_handle_id(&handle_id).map_err(ApiError::bad)?;
    tokio::spawn(async move { services::cache().remove(&handle_id).await })
        .await.map_err(ApiError::internal)?.map_err(ApiError::cache)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 从已发布清单中提供资源, 不允许读取实例描述或私有工作目录.
async fn playback_asset(
    Path((handle_id, asset_name)): Path<(String, String)>, request: Request,
) -> Result<Response, ApiError> {
    check_handle_id(&handle_id).map_err(ApiError::bad)?;
    if !process::media_filename(&asset_name) { return Err(ApiError::missing("playback asset not found")); }
    let spec = services::cache().spec(&handle_id).await.map_err(ApiError::cache)?;
    if source_path(&spec.media_id, services::database(), services::scanner()).await?.is_none() {
        return Err(ApiError::missing("cache source media is unavailable"));
    }
    let path = services::cache().media_path(&handle_id, &asset_name).await.map_err(ApiError::missing)?;
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

/// 按稳定顺序选择第一个仍存在且未越界的副本, 不为轮询执行 ffprobe.
async fn source_path(
    media_id: &str, database: &DatabaseManager, scanner: &ScannerManager,
) -> Result<Option<PathBuf>, ApiError> {
    for path in database.paths_by_media_id(media_id).await.map_err(ApiError::internal)? {
        match scanner.resolve(&path).await {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {},
            Err(error) => return Err(ApiError::media(format!("{error:#}"))),
        }
    }
    Ok(None)
}

/// 在 API 层聚合源路径, CacheManager 不依赖数据库或扫描管理器.
async fn cache_entry(snapshot: CacheSnapshot) -> Result<CacheEntry, ApiError> {
    let source_path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;
    Ok(CacheEntry { snapshot, source_path })
}

/// 构建和重试共用源身份检查, 内容改变时拒绝将旧句柄续接到新媒体.
async fn prepare_media(
    path: &FilePath, expected_media_id: Option<&str>, database: &DatabaseManager,
    scanner: &ScannerManager, processes: &ProcessManager,
) -> Result<(String, MediaInfo), ApiError> {
    let file = scanner.scan_one(database, path).await.map_err(ApiError::media)?
        .ok_or_else(|| ApiError::missing("media file not found"))?;
    if expected_media_id.is_some_and(|media_id| media_id != file.media_id) {
        return Err(ApiError::media("cache source content changed; build a new cache"));
    }
    let metadata = processes.probe(&file.path).await.map_err(ApiError::media)?;
    let (media_id, configs) = database.get_file_by_path(&file.path).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("media path was removed during cache construction"))?;
    if file.media_id != media_id {
        return Err(ApiError::media("media path changed during cache construction; retry the request"));
    }
    Ok((media_id, MediaInfo { metadata, configs }))
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
