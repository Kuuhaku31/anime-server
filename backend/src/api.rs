
// backend/src/api.rs
// 聚合全局 Manager 并公开 HTTP API 契约.

use std::path::{Path as FilePath, PathBuf};

use axum::{
    body::Body, extract::{rejection::JsonRejection, Path, Request},
    http::{header, HeaderValue, StatusCode}, response::{IntoResponse, Response},
    routing::{get, post, put}, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::services::ServeFile;

use crate::{
    cache::CacheSnapshot, constants::CACHE_VERSION, dandanplay::{self, DanmakuError}, db::DatabaseManager, models::{
        CacheSpec,
        MediaConfigs,
        MediaMetadata,
        PlaybackQuery,
        ScanChanges,
    }, process::{self, ProcessManager}, scanner::ScannerManager, services,
};


/// 按路径完整替换媒体播放配置的请求.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveMediaConfigsRequest {
    /// 容器内媒体路径.
    pub(crate) path: PathBuf,
    /// 要完整保存的播放位置和流选择.
    pub(crate) configs: MediaConfigs,
}

/// 单独替换媒体内容的剧集 ID, 显式 null 用于清空关联.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveMediaEpisodeRequest {
    /// 正整数剧集 ID 或 null, 不允许省略字段而意外清空.
    #[serde(deserialize_with = "Option::deserialize")]
    episode_id: Option<i64>,
}

/// 按剧集手动加载弹幕, refresh 为 true 时绕过已有缓存.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadDanmakuRequest {
    /// 浏览器可无损表示的正整数 DandanPlay 剧集 ID.
    episode_id: u64,
    /// 是否显式要求重新访问上游, 缺省使用已有缓存.
    #[serde(default)]
    refresh: bool,
}

// 各项 JSON 响应类型
type GetConfigResponse = Json<ConfigResponseBody>;
type ListMediaResponse = Json<Vec<ListMediaResponseItem>>;
type MediaMetadataResponse = Json<MediaMetadataResponseBody>;
type ScanMediaResponse = Json<ScanChanges>;
type RescanMediaResponse = Json<Option<ListMediaResponseItem>>;
type SaveMediaConfigsResponse = StatusCode;
/// 返回手动保存后的内容标识和剧集 ID.
type SaveMediaEpisodeResponse = Json<MediaEpisodeResponseBody>;
/// 返回识别后实际保存的 ID 和本次是否取得唯一匹配.
type IdentifyMediaResponse = Json<IdentifyMediaResponseBody>;
/// 直接返回完整时间有序弹幕数组, 不附带包装对象.
type LoadDanmakuResponse = Json<Vec<dandanplay::DanmakuComment>>;
type BuildCacheResponse = Json<BuildCacheResponseBody>;
type ListCacheResponse = Json<CacheListResponseBody>;
type CacheDetailResponse = Json<CacheSnapshot>;
type StopCacheResponse = CacheDetailResponse;
type DeleteCacheResponse = StatusCode;
type PlaybackAssetResponse = Response;

/// 描述前端初始化所需的公开配置.
#[derive(Debug, Serialize)]
struct ConfigResponseBody {
    /// 网页是否默认启用深色模式.
    default_dark_mode: bool,
}

/// 描述媒体列表中数据库登记的单个路径及共享配置.
#[derive(Debug, Serialize)]
struct ListMediaResponseItem {
    /// 内容记录 ID, 对应数据库 media_id.
    file_id: String,
    /// 容器内媒体路径.
    path: PathBuf,
    /// 同内容路径共享的播放配置.
    configs: MediaConfigs,
    /// 同内容路径共享的 DandanPlay 剧集 ID, null 表示尚未关联.
    episode_id: Option<i64>,
}

/// 描述独立于播放配置的剧集关联.
#[derive(Debug, Serialize)]
struct MediaEpisodeResponseBody {
    /// 数据库中的媒体内容标识.
    file_id: String,
    /// 当前保存的剧集 ID, 可由用户清空.
    episode_id: Option<i64>,
}

/// 区分本次唯一匹配与未匹配时保留的已有剧集关联.
#[derive(Debug, Serialize)]
struct IdentifyMediaResponseBody {
    /// 本次重新扫描确认的媒体内容标识.
    file_id: String,
    /// 数据库实际保存的剧集 ID, 无唯一结果时保留原关联.
    episode_id: Option<i64>,
    /// 本次识别是否取得并保存了唯一精确匹配.
    matched: bool,
}

/// 描述单个媒体实时探测得到的 metadata 和共享配置.
#[derive(Debug, Serialize)]
struct MediaMetadataResponseBody {
    /// 本次探测得到的媒体元数据.
    metadata: MediaMetadata,
    /// 探测时读取的共享播放配置.
    configs: MediaConfigs,
}

/// 描述缓存构建结果及媒体探测信息.
#[derive(Debug, Serialize)]
struct BuildCacheResponseBody {
    /// 新建或复用的缓存条目.
    cache: CacheSnapshot,
    /// 当前路径重新探测得到的媒体元数据.
    media_meta: MediaMetadata,
    /// 后端读取的共享播放配置.
    media_configs: MediaConfigs,
}

/// 描述全部缓存条目的列表响应.
#[derive(Debug, Serialize)]
struct CacheListResponseBody {
    /// 按 handle_id 排序的缓存条目.
    entries: Vec<CacheSnapshot>,
}


/// 组装业务路由, 不向 Axum 注入业务 AppState.
pub(crate) fn router() -> Router {
    Router::new()
    .route("/api/config", get(get_config))

    .route("/api/media", get(list_media))
    .route("/api/media/{file_id}/metadata", get(media_metadata))
    .route("/api/media/{file_id}/episode", put(save_media_episode))
    .route("/api/media/scan", post(scan_media))
    .route("/api/media/rescan", post(rescan_media))
    .route("/api/media/identify", post(identify_media))
    .route("/api/media/configs", put(save_media_configs))

    .route("/api/danmaku", post(load_danmaku))

    .route("/api/cache", get(list_cache).post(build_cache))
    .route("/api/cache/{handle_id}", get(cache_detail).delete(delete_cache))
    .route("/api/cache/{handle_id}/retry", post(retry_cache))
    .route("/api/cache/{handle_id}/stop", post(stop_cache))

    .route("/playback/{handle_id}/{asset_name}", get(playback_asset))
}

/// 提供前端初始化所需的公开配置
async fn get_config() -> GetConfigResponse {
    Json(ConfigResponseBody { default_dark_mode: services::config().default_dark_mode })
}

/// 只读取数据库中的路径, 播放配置和剧集 ID.
async fn list_media()
-> Result<ListMediaResponse, ApiError>
{
    let records = services::database().list_paths_with_configs().await.map_err(ApiError::internal)?;
    Ok(records.into_iter().map(|(file_id, path, configs, episode_id)|
        ListMediaResponseItem { file_id, path, configs, episode_id }).collect::<Vec<_>>().into())
}

/// 按数据库内容 ID 探测一个仍存在的媒体副本.
async fn media_metadata(Path(file_id): Path<String>) -> Result<MediaMetadataResponse, ApiError> {
    let paths = services::database().paths_with_configs_by_id(&file_id).await.map_err(ApiError::internal)?;
    if paths.is_empty() { return Err(ApiError::missing("media file not found")); }

    for (path, configs) in paths {
        match services::scanner().resolve(&path).await {
            Ok(resolved) => {
                let metadata = services::process().probe(&resolved).await.map_err(ApiError::media)?;
                return Ok(Json(MediaMetadataResponseBody { metadata, configs }));
            }
            Err(error) if error.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(ApiError::media(format!("{error:#}"))),
        }
    }
    Err(ApiError::missing("media file is unavailable"))
}

/// 请求扫描器同步全部媒体路径.
async fn scan_media() -> Result<ScanMediaResponse, ApiError> {
    services::scanner().scan_all(services::database()).await.map(Json).map_err(ApiError::internal)
}


/// 重扫单路径并返回更新后的数据库条目, 文件已删除时返回 null.
async fn rescan_media(input: Result<Json<PathBuf>, JsonRejection>)
-> Result<RescanMediaResponse, ApiError>
{
    let Json(input) = input.map_err(ApiError::bad)?;
    let Some(file) = services::scanner().scan_one(services::database(), &input).await.map_err(ApiError::media)?
        else { return Ok(Json(None)); };
    let (file_id, configs) = services::database().get_file_by_path(&file.path).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("media path was removed during rescan"))?;
    let episode_id = services::database().episode_id(&file_id).await.map_err(ApiError::internal)?;
    Ok(Json(Some(ListMediaResponseItem { file_id, path: file.path, configs, episode_id })))
}


/// 校验配置后保存
async fn save_media_configs(input: Result<Json<SaveMediaConfigsRequest>, JsonRejection>)
-> Result<SaveMediaConfigsResponse, ApiError>
{
    let Json(SaveMediaConfigsRequest { path, configs }) = input.map_err(ApiError::bad)?;
    if configs.position.is_some_and(|position| !position.is_finite() || position < 0.0) {
        return Err(ApiError::bad("position must be finite and non-negative"));
    }
    if [configs.video_stream_index, configs.audio_stream_index, configs.subtitle_stream_index]
        .into_iter().flatten().any(|index| index < 0) {
        return Err(ApiError::bad("stream indices must be non-negative"));
    }
    services::database().save_configs_by_path(&path, &configs).await.map_err(|error| {
        if matches!(error, sqlx::Error::RowNotFound) { ApiError::missing(error) }
        else { ApiError::internal(error) }
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// 手动保存或清空共享剧集 ID, 不触发扫描, 探测或外部请求.
async fn save_media_episode(
    Path(file_id): Path<String>, input: Result<Json<SaveMediaEpisodeRequest>, JsonRejection>,
) -> Result<SaveMediaEpisodeResponse, ApiError> {
    let Json(input) = input.map_err(ApiError::bad)?;
    let episode_id = services::database().save_episode_id(&file_id, input.episode_id).await.map_err(|error| {
        if matches!(error, sqlx::Error::RowNotFound) { ApiError::missing("media file not found") }
        else { ApiError::internal(error) }
    })?;
    Ok(Json(MediaEpisodeResponseBody { file_id, episode_id }))
}

/// 根据路径重复识别剧集, 唯一精确匹配覆盖原 ID, 未匹配时保留原关联.
async fn identify_media(input: Result<Json<PathBuf>, JsonRejection>) -> Result<IdentifyMediaResponse, ApiError> {
    let Json(path) = input.map_err(ApiError::bad)?;
    let database = services::database();
    let scanner = services::scanner();
    let file = scanner.scan_one(database, &path).await.map_err(ApiError::media)?
        .ok_or_else(|| ApiError::missing("media file not found"))?;
    let config = services::config().dandanplay.as_ref().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message: "DandanPlay identification is not configured; set DANDAN_APP_ID and DANDAN_APP_SECRET in the server environment".to_owned(),
    })?;
    let resolved = scanner.resolve(&file.path).await.map_err(ApiError::media)?;
    let file_size = tokio::fs::metadata(&resolved).await.map_err(ApiError::media)?.len();
    let file_name = file.path.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| ApiError::bad("media filename must be valid UTF-8"))?;
    let result = dandanplay::match_file(config, file_name, &file.media_id, file_size).await
        .map_err(ApiError::upstream)?;

    // 外部请求期间不占用扫描锁, 返回后重新确认路径内容再执行条件更新.
    let current = scanner.scan_one(database, &path).await.map_err(ApiError::media)?
        .ok_or_else(|| ApiError::conflict("media file disappeared during identification; refresh the list"))?;
    let resolved = scanner.resolve(&current.path).await.map_err(ApiError::media)?;
    let current_size = tokio::fs::metadata(resolved).await.map_err(ApiError::media)?.len();
    if current.media_id != file.media_id || current_size != file_size {
        return Err(ApiError::conflict("media file changed during identification; refresh the list and retry"));
    }
    let matched_id = result.map(i64::try_from).transpose().map_err(|error| ApiError::upstream(error.into()))?;
    let episode_id = database.save_identified_episode_id(&file.media_id, &path, matched_id).await.map_err(|error| {
        if matches!(error, sqlx::Error::RowNotFound) {
            ApiError::conflict("media path changed during identification; refresh the list and retry")
        } else { ApiError::internal(error) }
    })?;
    Ok(Json(IdentifyMediaResponseBody { file_id: file.media_id, episode_id, matched: result.is_some() }))
}

/// 校验弹幕请求并读取或刷新文件缓存, 不改动数据库或播放任务.
async fn load_danmaku(input: Result<Json<LoadDanmakuRequest>, JsonRejection>)
-> Result<LoadDanmakuResponse, ApiError>
{
    let Json(input) = input.map_err(ApiError::bad)?;
    let config = services::config().dandanplay.as_ref().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message: "DandanPlay identification is not configured; \
        set DANDAN_APP_ID and DANDAN_APP_SECRET in the server environment"
        .to_owned(),
    })?;
    dandanplay::load_danmaku(config, input.episode_id, input.refresh).await
    .map(Json).map_err(|error| match error {
        DanmakuError::Cache(error) => ApiError::internal(format!("{error:#}")),
        DanmakuError::Upstream(error) => ApiError::upstream(error),
    })
}


/// 接受 PlaybackQuery 并启动或复用转码, 不创建播放映射或等待编码完成.
async fn build_cache(input: Result<Json<PlaybackQuery>, JsonRejection>)
-> Result<BuildCacheResponse, ApiError>
{
    let Json(query) = input.map_err(ApiError::bad)?;
    let (media_id, media, media_configs) = prepare_media(&query.path, services::database(),
        services::scanner(), services::process()).await?;

    // 构建实际缓存配置
    let spec = {
        let query: &PlaybackQuery = &query;
        let metadata: &MediaMetadata = &media;
        let configs: &MediaConfigs = &media_configs;
        let video_stream_index = query.video_stream_index.or(configs.video_stream_index)
            .or_else(|| metadata.video_streams.first().map(|stream| stream.stream_index))
            .ok_or_else(|| ApiError::media("media has no playable video stream"))?;
        let audio_stream_index = query.audio_stream_index.unwrap_or_else(|| configs.audio_stream_index.or_else(||
            metadata.audio_streams.iter().find(|stream| stream.is_default)
                .or(metadata.audio_streams.first()).map(|stream| stream.stream_index)));
        let subtitle_stream_index = query.subtitle_stream_index.unwrap_or_else(|| configs.subtitle_stream_index.or_else(||
            metadata.subtitle_streams.iter().find(|stream| stream.is_default || stream.is_forced)
                .map(|stream| stream.stream_index)));
        Ok(CacheSpec::new(media_id, video_stream_index, audio_stream_index, subtitle_stream_index, CACHE_VERSION.to_string()))
    }?;
    let mut snapshot = services::cache().build(spec, query.path, media.clone(),
        services::config(), services::process()).await.map_err(ApiError::cache)?;
    snapshot.source_path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;
    Ok(Json(BuildCacheResponseBody { cache: snapshot, media_meta: media, media_configs }))
}

/// 获取全部缓存及转码 worker 占用合计, 不探测媒体或启动任务.
async fn list_cache()
-> Result<ListCacheResponse, ApiError>
{
    let snapshots = services::cache().list().await.map_err(ApiError::cache)?;
    let mut entries = Vec::with_capacity(snapshots.len());
    let mut sources: std::collections::HashMap<String, Option<PathBuf>> = std::collections::HashMap::new();
    for mut snapshot in snapshots {
        snapshot.source_path =
        if let Some(path) = sources.get(&snapshot.spec.media_id) { path.clone() }
        else {
            let path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;
            sources.insert(snapshot.spec.media_id.clone(), path.clone());
            path
        };
        entries.push(snapshot);
    }
    Ok(Json(CacheListResponseBody { entries }))
}

/// 查询单个句柄状态
async fn cache_detail(Path(handle_id): Path<String>)
-> Result<CacheDetailResponse, ApiError>
{
    let snapshot = services::cache().status(&handle_id).await.map_err(ApiError::cache)?;
    Ok(Json(snapshot))
}

/// 重新校验源文件身份和全部实际流
async fn retry_cache(Path(handle_id): Path<String>)
-> Result<BuildCacheResponse, ApiError>
{
    // 获取缓存实例的规格信息
    let spec = services::cache().spec(&handle_id).await.map_err(ApiError::cache)?;

    // 找到一个源文件路径
    let source = source_path(&spec.media_id, services::database(), services::scanner()).await?
    .ok_or_else(|| ApiError::missing("cache source media is unavailable"))?;

    // 准备媒体信息和配置
    let (_, media, media_configs) = prepare_media(&source, services::database(),
    services::scanner(), services::process()).await?;

    let mut snapshot = services::cache().retry(&handle_id, source, media.clone(),
    services::config(), services::process()).await.map_err(ApiError::cache)?;
    snapshot.source_path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;

    Ok(Json(BuildCacheResponseBody { cache: snapshot, media_meta: media, media_configs }))
}

/// 停止缓存实例生成
async fn stop_cache(Path(handle_id): Path<String>)
-> Result<StopCacheResponse, ApiError>
{
    let mut snapshot = 
    tokio::spawn(async move { services::cache().stop(&handle_id).await })
    .await.map_err(ApiError::internal)?.map_err(ApiError::cache)?;
    snapshot.source_path = source_path(&snapshot.spec.media_id, services::database(), services::scanner()).await?;
    Ok(Json(snapshot))
}

/// 删除缓存实例
async fn delete_cache(Path(handle_id): Path<String>) -> Result<DeleteCacheResponse, ApiError> {
    tokio::spawn(async move { services::cache().remove(&handle_id).await })
    .await.map_err(ApiError::internal)?.map_err(ApiError::cache)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 获取播放资源
async fn
playback_asset(Path((handle_id, asset_name)): Path<(String, String)>, request: Request)
-> Result<PlaybackAssetResponse, ApiError>
{
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

// ========================================================================== //

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

/// 提供文件路径, 扫描并获取媒体信息
async fn prepare_media(
    path: &FilePath,
    database: &DatabaseManager,
    scanner: &ScannerManager,
    processes: &ProcessManager,
) -> Result<(String, MediaMetadata, MediaConfigs), ApiError>
{
    let file = scanner.scan_one(database, path).await.map_err(ApiError::media)?
        .ok_or_else(|| ApiError::missing("media file not found"))?;
    let metadata = processes.probe(&file.path).await.map_err(ApiError::media)?;
    let (media_id, configs) = database.get_file_by_path(&file.path).await.map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("media path was removed during cache construction"))?;
    if file.media_id != media_id {
        return Err(ApiError::media("media path changed during cache construction; retry the request"));
    }
    Ok((media_id, metadata, configs))
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

    /// 表示媒体, 缓存句柄或资源不存在.
    fn missing(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: error.to_string() }
    }

    /// 表示识别期间源路径发生变化, 调用方必须刷新后再尝试.
    fn conflict(error: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::CONFLICT, message: error.to_string() }
    }

    /// 保留外部 API 的超时语义, 其他网络和协议错误返回 502.
    fn upstream(error: anyhow::Error) -> Self {
        let timeout = error.downcast_ref::<reqwest::Error>().is_some_and(|error| error.is_timeout())
            || error.downcast_ref::<tokio::time::error::Elapsed>().is_some();
        Self {
            status: if timeout { StatusCode::GATEWAY_TIMEOUT } else { StatusCode::BAD_GATEWAY },
            message: format!("{error:#}"),
        }
    }

    /// 缓存操作保留缺失和停止冲突语义, 其他错误明确报告为内部失败.
    fn cache(error: anyhow::Error) -> Self {
        match error.downcast_ref::<crate::cache::CacheError>() {
            Some(crate::cache::CacheError::NotFound) => Self::missing(error),
            Some(crate::cache::CacheError::Stopping) => Self { status: StatusCode::CONFLICT, message: error.to_string() },
            None => Self::internal(format!("{error:#}")),
        }
    }
}

impl IntoResponse for ApiError {
    /// 统一记录并序列化 API 错误, 不提供成功形状的默认数据.
    fn into_response(self) -> Response {
        tracing::warn!(status = self.status.as_u16(), error = self.message, "API request failed");
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}


#[cfg(test)]
#[path = "../tests/api/playback.rs"]
mod tests;
