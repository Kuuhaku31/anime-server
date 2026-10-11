
// backend/src/video/scanner.rs
//! 扫描媒体目录并同步文件索引.

use std::{collections::HashMap, path::{Path, PathBuf}};
use anyhow::Result;
use walkdir::WalkDir;

use crate::{
    constants::{self, EXTENSIONS}, models::{ScanChanges, ScannedFile},
};


/// 手动和定时扫描共用互斥锁, 只有完整收集成功后才能替换数据库路径快照.
pub(crate) async fn scan_once(paths_in_db: Vec<PathBuf>) -> Result<ScanChanges, String> {

    // 同步目录遍历和新文件哈希放到阻塞线程池, 完整收集失败时不提交任何变更.
    let changes = tokio::task::spawn_blocking(move || -> Result<ScanChanges, String> {

        // 规范化扫描根目录, 即使调用方传入相对地址也只写入绝对目录.
        let root = constants::video_root_path();
        let root = std::fs::canonicalize(root).map_err(|e| format!("cannot resolve media directory: {e}"))?;

        let paths = paths_in_db.into_iter();
        let mut known: HashMap<_, _> = paths.map(|path| (path.clone(), path)).collect();
        let mut added = Vec::new();
        let mut count = 0; // 所有现存视频路径数
        for entry in WalkDir::new(&root) {
            let entry = entry.map_err(|e| format!("failed to read media directory entry: {e}"))?;
            let absolute_path = entry.path();
            if !entry.file_type().is_file() || !is_video(absolute_path) { continue; }
            count += 1;
            // 地址相同即认为未变, 即使内容或大小改变也不重算哈希, 符合增量扫描契约.
            if known.remove(absolute_path).is_none() {
                added.push(ScannedFile::new(absolute_path)?);
            }
        }

        let changes = ScanChanges { added, removed: known.into_values().collect(), count };

        Ok(changes)

    }).await.map_err(|e| format!("failed to join scan thread: {e}"))??;

    Ok(changes)
}


/// 强制重新读取单个地址, 不沿用全目录增量扫描的跳过哈希策略.
pub(crate) async fn scan_file(path: &Path) -> Result<Option<ScannedFile>, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || scan_file_at(constants::video_root_path(), &path))
        .await.map_err(|error| format!("failed to join scan thread: {error}"))?
}

// 保留数据库中的原地址, 仅规范化目标用于校验, 避免符号链接产生额外路径记录.
fn scan_file_at(root: &Path, path: &Path) -> Result<Option<ScannedFile>, String> {
    let root = std::fs::canonicalize(root).map_err(|error| format!("cannot resolve media directory: {error}"))?;
    if !path.is_absolute() || !path.starts_with(&root)
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
        || !is_video(path)
    {
        return Err("invalid media file path".to_string());
    }
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(format!("cannot resolve media file: {error}")),
    };
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err("media path must resolve to a file inside media directory".to_string());
    }
    let file = ScannedFile::new(path)?;
    Ok(Some(file))
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
    async fn single_file_scan_rehashes_content_and_removes_missing_paths() {
        // 隔离测试目录, 验证强制重算, 消失路径和越界地址, 不访问真实媒体挂载.
        let root = std::env::temp_dir().join(format!("anime-single-scan-{}", crate::video::cache::random_id().unwrap()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("episode.mkv");
        std::fs::write(&path, b"first").unwrap();
        let first = scan_file_at(&root, &path).unwrap().unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../sql/init.sql")).execute(&pool).await.unwrap();
        crate::db::repository::upsert_file(&pool, &first).await.unwrap();
        let old_id = first.pre16mib_md5().to_owned();
        crate::db::repository::save_progress(&pool, &old_id, 12.5).await.unwrap();
        // 另一个内容相同的副本不应被该地址的强制扫描删除.
        let sibling = root.join("copy.mkv");
        std::fs::write(&sibling, b"first").unwrap();
        crate::db::repository::upsert_file(&pool, &scan_file_at(&root, &sibling).unwrap().unwrap()).await.unwrap();
        #[cfg(unix)]
        {
            // 拒绝越界符号链接, 根目录内链接仍按原数据库地址更新.
            let link = root.join("link.mkv");
            std::os::unix::fs::symlink(root.parent().unwrap(), &link).unwrap();
            assert!(scan_file_at(&root, &link).is_err());
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&sibling, &link).unwrap();
            let linked = scan_file_at(&root, &link).unwrap().unwrap();
            assert_eq!(linked.path(), &link);
            std::fs::remove_file(link).unwrap();
        }
        assert_eq!(first.path(), &path);
        std::fs::write(&path, b"replaced media").unwrap();
        let second = scan_file_at(&root, &path).unwrap().unwrap();
        assert_ne!(first.pre16mib_md5(), second.pre16mib_md5());
        assert_eq!(second.size(), 14);
        crate::db::repository::upsert_file(&pool, &second).await.unwrap();
        assert_eq!(crate::db::repository::get_video_paths_by_id(&pool, &old_id).await.unwrap(), vec![sibling.clone()]);
        assert_eq!(crate::db::repository::get_progress(&pool, &old_id).await.unwrap().unwrap().watch_pos, Some(12.5));
        let new_id = second.pre16mib_md5();
        assert_eq!(crate::db::repository::get_video_paths_by_id(&pool, new_id).await.unwrap(), vec![path.clone()]);
        assert!(scan_file_at(&root, &root.join("../outside.mkv")).is_err());
        assert!(scan_file_at(&root, Path::new("episode.mkv")).is_err());
        assert!(scan_file_at(&root, &root.join("ignored.txt")).is_err());
        std::fs::remove_file(&path).unwrap();
        let removed = scan_file_at(&root, &path).unwrap();
        // 当前单文件扫描用 None 表示文件消失, 调用方单独删除数据库地址.
        assert!(removed.is_none());
        crate::db::repository::delete_path(&pool, &path).await.unwrap();
        assert_eq!(crate::db::repository::get_paths(&pool).await.unwrap(), vec![sibling.clone()]);
        assert!(crate::db::repository::get_progress(&pool, new_id).await.unwrap().is_some());
        std::fs::remove_file(sibling).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn initial_track_preferences_match_server_defaults() {
        // 默认轨道由播放配置生成器选择, 扫描模块不再提供独立选择函数.
        use crate::video::ffmpeg::{EncodeSpec, PlaybackRequest};
        let (info, request) = crate::video::ffmpeg::fixture();
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
    async fn scans_return_full_paths_and_reuse_known_files() {
        // 当前入口固定扫描 /media, 只读现有媒体; 无挂载时验证明确的根目录错误.
        if let Err(error) = std::fs::canonicalize(constants::video_root_path()) {
            let expected = format!("cannot resolve media directory: {error}");
            assert_eq!(scan_once(Vec::new()).await.err().unwrap(), expected);
            return;
        }
        let first = scan_once(Vec::new()).await.unwrap();
        assert_eq!(first.count, first.added.len() as u64);
        assert!(first.removed.is_empty());
        assert!(first.added.iter().all(|file| file.path().is_absolute() && is_video(file.path())));
        let known = first.added.iter().map(|file| file.path().clone()).collect::<Vec<_>>();
        let unchanged = scan_once(known.clone()).await.unwrap();
        assert_eq!(unchanged.count, first.count);
        assert!(unchanged.added.is_empty());
        assert!(unchanged.removed.is_empty());

        // 根目录外的虚拟旧地址不会匹配扫描结果, 应出现在删除列表中.
        let stale = std::env::temp_dir().join(format!("anime-stale-{}.mkv", std::process::id()));
        let mut known = known;
        known.push(stale.clone());
        let removed = scan_once(known).await.unwrap();
        assert_eq!(removed.count, first.count);
        assert!(removed.added.is_empty());
        assert_eq!(removed.removed, vec![stale]);
    }
}
