//! 扫描媒体目录并同步文件索引.

use std::{collections::HashSet, path::{Path, PathBuf}};
use anyhow::Result;
use tracing::info;
use walkdir::WalkDir;

use crate::{
    AppState, constants::{self, EXTENSIONS}, db, models::{ScanChanges, ScannedFile},
};

#[cfg(test)]
#[path = "tests/scanner.rs"]
mod tests;

/// 扫描媒体目录, 同步数据库索引, 返回扫描摘要:
///
/// 遍历媒体目录, 对每个视频文件:
///  - 检查该文件地址是否在数据库 file_path 表内
///  - 如果不在数据库 file_path 表内, 计算文件前 16MiB 的 MD5 哈希, 并将文件地址和哈希写入数据库
///  - 如果在数据库内, 检查文件大小是否变化, 如果变化则重新计算哈希并更新数据库
///  - 统计好实际导致插入更新数据库的文件地址数量
///  - 获得一个媒体目录下所有视频文件地址列表
/// 获取数据库 file_path 表所有地址, 和媒体目录下所有视频文件地址列表进行比对,
/// 删除 file_path 表内存在但媒体目录下不存在的地址, 统计删除数量.
/// 最后返回一个 ScanChanges 结构体, 包含新增, 删除和总计的文件数量.
pub(crate) async fn scan_root(state: &AppState) -> Result<ScanChanges, String> {

    // 整轮扫描持有锁, 防止手动和后台扫描交错更新.
    info!("准备扫描媒体目录: {}", constants::video_root_path().display());
    let _scan = state.scan_lock.lock().await;
    info!("扫描媒体目录开始: {}", constants::video_root_path().display());

    let path_and_size_in_db = db::get_paths_and_size(&state.db).await
    .map_err(|error| format!("failed to read paths and sizes from database: {error}"))?;
    let root = tokio::fs::canonicalize(constants::video_root_path()).await
    .map_err(|error| format!("cannot resolve media directory: {error}"))?;

    let mut paths_in_folder = Vec::new();
    let mut added = 0;

    // 边遍历边写入, 哈希交给阻塞线程, 每个文件的两表更新独立提交.
    for entry in WalkDir::new(&root).sort_by_file_name() {

        let entry = entry.map_err(|error| format!("failed to read media directory entry: {error}"))?;
        let path = entry.path();

        if !entry.file_type().is_file() || !is_video(path) { continue; } // 只处理视频文件
        paths_in_folder.push(path.to_path_buf());
        let size = tokio::fs::metadata(path).await
        .map_err(|error| format!("failed to read media file metadata {}: {error}", path.display()))?.len();

        if path_and_size_in_db.get(path) == Some(&size) { continue; } // 同地址同大小不重算哈希

        // 单文件写入失败记录完整地址和错误后继续, 仅成功提交的地址计入 added.
        let file = match ScannedFile::new(path) {
            Ok(file) => file,
            Err(error) => {
                tracing::error!(path = %path.display(), "failed to scan media file: {error}");
                continue;
            },
        };

        match db::upsert_file(&state.db, &file).await {
            Ok(()) => added += 1,
            Err(error) => tracing::error!(path = %file.path().display(), "failed to update scanned file: {error}"),
        }
    }

    let mut removed = 0;
    let total = paths_in_folder.len() as u64;

    // 查找数据库中已知但在文件夹中不存在的路径, 作为删除列表.
    let paths_in_folder_set: HashSet<&Path> = paths_in_folder.iter().map(|p| p.as_path()).collect();
    let need_delete: Vec<PathBuf> = path_and_size_in_db.keys()
        .filter(|path| !paths_in_folder_set.contains(path.as_path()))
        .cloned()
        .collect();
    if !need_delete.is_empty() {
        db::delete_paths(&state.db, &need_delete).await.map_err(|error| error.to_string())?;
        removed = need_delete.len() as u64;
    }

    let changes = ScanChanges {
        added, removed, total,
    };

    info!("扫描媒体目录完成: {}, 添加: {}, 删除: {}, 总计: {}", constants::video_root_path().display(), added, removed, total);
    Ok(changes)
}


/// 强制重新读取单个地址, 不沿用全目录增量扫描的跳过哈希策略.
pub(crate) async fn scan_file(state: &AppState, path: &Path) -> Result<Option<ScannedFile>, String> {

    let p = path.to_owned();
    let file = tokio::task::spawn_blocking(move || -> Result<Option<ScannedFile>, String> {
    let path = &p;
    let root = std::fs::canonicalize(constants::video_root_path()).map_err(|error| format!("cannot resolve media directory: {error}"))?;
    if !path.is_absolute() || !path.starts_with(&root)
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
        || !is_video(path)
    {
        return Err(format!("invalid media file path: {:?}", path));
    }

    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(format!("cannot resolve media file: {error}")),
    };
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err(format!("media path must resolve to a file inside media directory: {:?}", path));
    }
    Ok(Some(ScannedFile::new(path)?))
    }).await.map_err(|error| format!("failed to join scan thread: {error}"))??;

    // 如果扫描结果为 None, 说明文件不存在或不在媒体目录内, 删除数据库中对应路径记录.
    // 如果扫描结果为 Some, 则将扫描结果写入数据库, 覆盖原有路径关联.
    if let Some(f) = file.as_ref() {
        info!("服务端完成单文件扫描请求: {:?}, hash: {:?}", path, f.pre16mib_md5());
        db::upsert_file(&state.db, f).await.map_err(|error| error.to_string())?;
    } else {
        info!("服务端完成单文件扫描请求: {:?}, 文件不存在或不在媒体目录内", path);
        db::delete_path(&state.db, path).await.map_err(|error| error.to_string())?;
    };

    Ok(file)
}

fn is_video(path: &Path) -> bool {
    path.extension()
    .and_then(|value| value.to_str())
    .map(|value| {
        EXTENSIONS
        .iter().any(|extension| value.eq_ignore_ascii_case(extension))
    })
    .unwrap_or(false)
}
