
// backend/src/api.rs
//! 定义业务 API 路由和处理函数.
//! 提供文件查询, 观看进度, 媒体扫描和播放 API 路由.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use crate::{
    AppState, db::repository, models::ProgressInput, scanner,
};

mod playback;

/// 构建业务 API 路由.
pub fn router(state: AppState) -> Router {
    Router::new()
    .route("/api/files", get(list_files))
    .route("/api/files/{pre16mib_md5}", get(get_file))
    .route("/api/files/{pre16mib_md5}/identify", post(identify_file))
    .route("/api/scan/file", post(scan_file))
    .route("/api/files/{pre16mib_md5}/progress", get(get_progress).put(save_progress),)
    .route("/api/config", get(get_config))
    .route("/api/scan", post(scan))
    .merge(playback::router())
    .with_state(state) // 将应用状态传递给所有路由处理函数
}

/// 获取 Web 前端配置.
async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.config.web.clone()))
}

/// 获取全部视频记录.
async fn list_files(State(state): State<AppState>) -> impl IntoResponse {
    match repository::get_video_files(&state.db).await {
        Ok(items) => (StatusCode::OK, Json(items)).into_response(),
        Err(error) => internal_error(error),
    }
}

/// 获取视频记录及其所有存储路径.
async fn get_file(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    let video = match repository::get_video_by_id(&state.db, &pre16mib_md5).await {
        Ok(Some(video)) => video,
        Ok(None) => return not_found(),
        Err(error) => return internal_error(error),
    };

    let paths = match repository::get_video_paths_by_id(&state.db, &pre16mib_md5).await {
        Ok(paths) => paths,
        Err(error) => return internal_error(error),
    };

    (
        StatusCode::OK,
        Json(json!({
            "video": video,
            "paths": paths,
        })),
    )
        .into_response()
}

/// 当前识别仅写入演示剧集 ID, 不调用真实 DandanPlay 服务.
async fn identify_file(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    match repository::set_mock_identification(&state.db, &pre16mib_md5).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "mode": "mock",
                "ddp_epi_id": 1,
            })),
        )
            .into_response(),
        Err(sqlx::Error::RowNotFound) => not_found(),
        Err(error) => internal_error(error),
    }
}

/// 获取视频观看进度.
async fn get_progress(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
) -> impl IntoResponse {
    match repository::get_progress(&state.db, &pre16mib_md5).await {
        Ok(Some(progress)) => (StatusCode::OK, Json(progress)).into_response(),
        Ok(None) => not_found(),
        Err(error) => internal_error(error),
    }
}

/// 保存视频观看进度.
async fn save_progress(
    State(state): State<AppState>,
    Path(pre16mib_md5): Path<String>,
    Json(input): Json<ProgressInput>,
) -> impl IntoResponse {
    if !input.position.is_finite() || input.position < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "position must be finite and non-negative"
            })),
        )
            .into_response();
    }

    match repository::save_progress(&state.db, &pre16mib_md5, input.position).await {
        Ok(()) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(sqlx::Error::RowNotFound) => not_found(),
        Err(error) => internal_error(error),
    }
}

/// 执行一次媒体目录扫描.
async fn scan(State(state): State<AppState>) -> impl IntoResponse {
    match scanner::scan_root(&state).await {
        Ok(change) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "scanned": change.total,
            })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// 单文件扫描请求, 服务器完整地址是唯一定位信息.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileScanInput {
    path: std::path::PathBuf,
}

/// 重新扫描指定记录的单个完整地址, 与全目录扫描共用锁并原子替换路径关联.
async fn scan_file(
    State(state): State<AppState>,
    Json(input): Json<FileScanInput>,
) -> axum::response::Response {

    info!("服务端收到单文件扫描请求: {:?}, 正在等待锁", input.path);
    let _scan = state.scan_lock.lock().await;
    info!("服务端获得扫描锁, 开始扫描: {:?}", input.path);

    // 扫描
    let file = match crate::scanner::scan_file(&state, &input.path).await {
        Ok(f) => f,
        Err(error) => {
            tracing::warn!("single file scan failed: {error}, path: {:?}", input.path);
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": error}))).into_response();
        },
    };


    let res = (StatusCode::OK, Json(json!({
        "ok": true, "scanned": usize::from(file.is_some()),
        "pre16mib_md5": file.as_ref().map(|f| f.pre16mib_md5()),
    }))).into_response();

    res
}

/// 构造 404 响应.
fn not_found() -> axum::response::Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
}

/// 构造 500 响应.
fn internal_error(error: impl std::fmt::Display) -> axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": error.to_string()}))).into_response()
}
