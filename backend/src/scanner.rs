
// backend/src/scanner.rs
//! 扫描媒体目录并同步文件索引.

use std::{collections::HashSet, path::{Path, PathBuf}};
use anyhow::Result;
use tracing::info;
use walkdir::WalkDir;

use crate::{
    AppState, constants::{self, EXTENSIONS}, db, models::{ScanChanges, ScannedFile},
};


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


// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scanned_files_rehash_content_and_preserve_other_paths() {
        // 隔离文件构造和数据库更新测试, 不调用已删除的可配置根目录扫描辅助函数.
        let root = std::env::temp_dir().join(format!("anime-single-scan-{}", crate::video::random_id().unwrap()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("episode.mkv");
        std::fs::write(&path, b"first").unwrap();
        let first = ScannedFile::new(&path).unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../sql/init.sql")).execute(&pool).await.unwrap();
        db::upsert_file(&pool, &first).await.unwrap();
        let old_id = first.pre16mib_md5().to_owned();
        db::save_progress(&pool, &old_id, 12.5).await.unwrap();
        // 另一个内容相同的副本不应被该地址的强制扫描删除.
        let sibling = root.join("copy.mkv");
        std::fs::write(&sibling, b"first").unwrap();
        db::upsert_file(&pool, &ScannedFile::new(&sibling).unwrap()).await.unwrap();
        #[cfg(unix)]
        {
            // 文件构造保留符号链接原地址, 不替换为目标地址.
            let link = root.join("link.mkv");
            std::os::unix::fs::symlink(&sibling, &link).unwrap();
            let linked = ScannedFile::new(&link).unwrap();
            assert_eq!(linked.path(), &link);
            std::fs::remove_file(link).unwrap();
        }
        assert_eq!(first.path(), &path);
        std::fs::write(&path, b"replaced media").unwrap();
        let second = ScannedFile::new(&path).unwrap();
        assert_ne!(first.pre16mib_md5(), second.pre16mib_md5());
        assert_eq!(second.size(), 14);
        db::upsert_file(&pool, &second).await.unwrap();
        assert_eq!(db::get_video_paths_by_id(&pool, &old_id).await.unwrap(), vec![sibling.clone()]);
        assert_eq!(db::get_progress(&pool, &old_id).await.unwrap().unwrap().watch_pos, Some(12.5));
        let new_id = second.pre16mib_md5();
        assert_eq!(db::get_video_paths_by_id(&pool, new_id).await.unwrap(), vec![path.clone()]);
        std::fs::remove_file(&path).unwrap();
        // 文件已消失时构造失败, 独立删除数据库地址仍保留业务记录.
        assert!(ScannedFile::new(&path).is_err());
        db::delete_path(&pool, &path).await.unwrap();
        assert_eq!(db::get_paths_and_size(&pool).await.unwrap().into_keys().collect::<Vec<_>>(), vec![sibling.clone()]);
        assert!(db::get_progress(&pool, new_id).await.unwrap().is_some());
        std::fs::remove_file(sibling).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn initial_track_preferences_match_server_defaults() {
        // 默认轨道由播放配置生成器选择, 扫描模块不再提供独立选择函数.
        use crate::video::{EncodeSpec, PlaybackRequest};
        let (info, request) = crate::video::fixture();
        let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
        assert_eq!((spec.config.audio_stream, spec.config.subtitle_stream), (Some(1), None));
        let info = serde_json::from_value(serde_json::json!({
            "duration": 20.0, "start_time": 0.0,
            "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
            "audio_tracks": [
                {"stream_index": 1, "codec_name": "aac", "is_default": false, "is_forced": false},
                {"stream_index": 4, "codec_name": "aac", "is_default": true, "is_forced": false}
            ],
            "subtitle_tracks": [
                {"stream_index": 3, "codec_name": "ass", "is_default": false, "is_forced": true}
            ],
            "attachments": []
        })).unwrap();
        let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, PlaybackRequest::default(), None).unwrap();
        assert_eq!((spec.config.audio_stream, spec.config.subtitle_stream), (Some(4), Some(3)));
    }

    #[test]
    fn video_extensions_are_case_insensitive() {
        // 只验证扩展名识别, 不创建或修改宿主机媒体文件.
        for extension in EXTENSIONS {
            assert!(is_video(Path::new(&format!("episode.{extension}"))));
            assert!(is_video(Path::new(&format!("episode.{}", extension.to_ascii_uppercase()))));
        }
        for path in ["episode", "episode.", "episode.txt", "episode.mkv.txt"] {
            assert!(!is_video(Path::new(path)));
        }
    }

    #[tokio::test]
    async fn scan_entry_points_report_errors_without_touching_real_media() {
        // 构造完整隔离状态, 固定 /media 入口只测试拒绝路径和数据库读取失败.
        use std::sync::Arc;
        use crate::{config::AppConfig, video::{CacheManager, MetadataCache,
            Processes, SessionManager}};
        let root = std::env::temp_dir().join(format!("anime-scan-state-{}", crate::video::random_id().unwrap()));
        std::fs::create_dir(&root).unwrap();
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        let processes = Processes::new(2, 16);
        let cache = CacheManager::new(root.clone(), config.playback.clone(), processes.clone()).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../sql/init.sql")).execute(&pool).await.unwrap();
        let state = AppState {
            db: pool, config: Arc::new(config), metadata: MetadataCache::new(processes),
            playback: SessionManager::new(cache, 60), scan_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let root_error = std::fs::canonicalize(constants::video_root_path()).err();
        for path in ["episode.mkv", "/media/../outside.mkv", "/media/ignored.txt"] {
            let error = scan_file(&state, Path::new(path)).await.err().unwrap();
            if let Some(root_error) = &root_error {
                assert_eq!(error, format!("cannot resolve media directory: {root_error}"));
            } else {
                assert!(error.starts_with("invalid media file path:"));
            }
        }
        assert!(db::get_paths_and_size(&state.db).await.unwrap().is_empty());
        // 关闭数据库确保全目录入口在读取路径时失败, 不遍历或更新真实媒体.
        state.db.close().await;
        assert!(scan_root(&state).await.err().unwrap().starts_with("failed to read paths and sizes from database:"));
        assert!(state.scan_lock.try_lock().is_ok());
        state.playback.shutdown().await.unwrap();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
