
// backend/src/video/scanner.rs
//! 扫描媒体目录并同步文件索引.

use std::{collections::HashMap, fs::File, io::Read, path::{Path, PathBuf}};
use anyhow::{Context, Result};
use md5::{Digest, Md5};
use tracing::warn;
use walkdir::WalkDir;

use crate::{
    models::{FilePath, ScannedFile},
    AppState,
    constants::VIDEO_ROOT,
    db::repository,
    video::metadata::MediaInfo,
    constants::{EXTENSIONS, HASH_LIMIT},
};


/// 手动和定时扫描共用互斥锁, 只有完整收集成功后才能替换数据库路径快照.
pub async fn scan_once(state: &AppState) -> Result<u64> {

    // 锁的有效期取决于 _scan 的作用域
    let _scan = state.scan_lock.lock().await; // 锁住扫描任务, 防止同时进行多个扫描任务

    // 锁覆盖路径读取, 目录比对和提交, 防止两个扫描使用同一份过期快照.
    let known = repository::list_paths(&state.db).await?;
    let root = PathBuf::from(VIDEO_ROOT);
    // 同步目录遍历和新文件哈希放到阻塞线程池, 完整收集失败时不提交任何变更.
    let mut changes = tokio::task::spawn_blocking({
        let root = root.clone();
        move || collect_files(&root, known)
    }).await??;
    // 新路径仅探测一次并记录默认轨, 探测失败仍保留文件扫描结果并明确记录警告.
    for file in &mut changes.added {
        let source = root.join(if file.path.is_empty() {
            format!("{}.{}", file.name_no_ext, file.extension)
        } else {
            format!("{}/{}.{}", file.path, file.name_no_ext, file.extension)
        });
        match state.metadata.get(&file.pre16mib_md5, &source, false).await {
            Ok(info) => {
                (file.audio_stream_index, file.subtitle_stream_index) = default_tracks(&info);
            }
            Err(error) => warn!(file_id = file.pre16mib_md5, "default track probe failed: {error:#}"),
        }
    }
    repository::sync_scanned_files(&state.db, &changes.added, &changes.removed).await?;
    Ok(changes.count)
}


// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

// count 仍是所有现存视频路径数, 而不是本轮新增数量或去重后的文件数量.
struct ScanChanges {
    added: Vec<ScannedFile>,
    removed: Vec<FilePath>,
    count: u64,
}

/// 遍历目录并消耗已知路径表, 只对新地址读取前缀 MD5, 剩余地址即失效路径.
fn collect_files(root: &Path, known: Vec<FilePath>) -> Result<ScanChanges> {
    if !root.exists() { anyhow::bail!("media directory is missing"); }

    let mut known: HashMap<_, _> = known.into_iter()
        .map(|file| (root.join(file.relative_path()), file)).collect();
    let mut added = Vec::new();
    let mut count = 0;
    for entry in WalkDir::new(root) {
        let entry = entry.context("failed to read media directory entry")?;
        let absolute_path = entry.path();
        if !entry.file_type().is_file() || !is_video(absolute_path) {
            continue;
        }
        count += 1;
        // 地址相同即认为未变, 即使内容或大小改变也不重算哈希, 符合增量扫描契约.
        if known.remove(absolute_path).is_none() {
            added.push(scan_file(root, absolute_path)?);
        }
    }

    Ok(ScanChanges { added, removed: known.into_values().collect(), count })
}


fn scan_file(root: &Path, absolute_path: &Path) -> Result<ScannedFile> {
    let metadata = std::fs::metadata(absolute_path)
        .with_context(|| format!("failed to read metadata for {}", absolute_path.display()))?;
    let relative_path = absolute_path
        .strip_prefix(root)
        .with_context(|| format!("failed to resolve path under {}", root.display()))?;
    let name_no_ext = absolute_path
        .file_stem()
        .context("video file has no file name")?
        .to_string_lossy()
        .into_owned();
    let extension = absolute_path
        .extension()
        .context("video file has no extension")?
        .to_string_lossy()
        .into_owned();
    let path = relative_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    Ok(ScannedFile {
        pre16mib_md5: hash_prefix(absolute_path)?,
        size: metadata.len() as i64,
        audio_stream_index: None,
        subtitle_stream_index: None,
        path,
        name_no_ext,
        extension,
    })
}

// 最多读取前 16 MiB, 短文件读到 EOF
fn hash_prefix(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut remaining = HASH_LIMIT;

    while remaining > 0 {
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))?;
        let bytes_read = file
            .read(&mut buffer[..read_limit])
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        remaining -= bytes_read as u64;
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn is_video(path: &Path) -> bool {
    path.extension()
    .and_then(|value| value.to_str())
    .map(|value| {
        EXTENSIONS
        .iter()
        .any(|extension| value.eq_ignore_ascii_case(extension))
    })
    .unwrap_or(false)
}

/// 新文件沿用点播默认策略: 默认音轨否则第一轨, 默认或强制字幕否则关闭.
fn default_tracks(info: &MediaInfo) -> (Option<i32>, Option<i32>) {
    let audio = info.audio_tracks.iter().find(|track| track.is_default)
        .or_else(|| info.audio_tracks.first()).map(|track| track.stream_index);
    let subtitle = info.subtitle_tracks.iter().find(|track| track.is_default || track.is_forced)
        .map(|track| track.stream_index);
    (audio, subtitle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn initial_track_preferences_match_server_defaults() {
        let (info, _) = crate::video::ffmpeg::fixture();
        assert_eq!(default_tracks(&info), (Some(1), None));
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
        assert_eq!(default_tracks(&info), (Some(4), Some(3)));
    }

    #[tokio::test]
    async fn incremental_scans_reuse_paths_and_preserve_progress_after_removal() {
        // 隔离媒体和内存数据库, 验证新增, 原地修改, 删除及改名后的业务记录复用.
        let root = std::env::temp_dir().join(format!("anime-scan-{}", crate::video::cache::random_id().unwrap()));
        std::fs::create_dir_all(root.join("series")).unwrap();
        std::fs::write(root.join("first.mkv"), b"original").unwrap();
        std::fs::write(root.join("series/copy.MKV"), b"original").unwrap();
        std::fs::write(root.join("ignored.txt"), b"not video").unwrap();
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();

        let mut first = collect_files(&root, repository::list_paths(&pool).await.unwrap()).unwrap();
        assert_eq!(first.count, 2);
        assert_eq!(first.added.len(), 2);
        let id = first.added[0].pre16mib_md5.clone();
        first.added[0].audio_stream_index = Some(1);
        first.added[0].subtitle_stream_index = Some(2);
        repository::sync_scanned_files(&pool, &first.added, &first.removed).await.unwrap();
        repository::save_progress(&pool, &id, 12.5).await.unwrap();
        repository::set_mock_identification(&pool, &id).await.unwrap();
        let initial = repository::get_video(&pool, &id).await.unwrap().unwrap();
        assert_eq!(initial.audio_stream_index, Some(1));
        assert_eq!(initial.subtitle_stream_index, Some(2));
        let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM file_path WHERE path = ''")
            .fetch_one(&pool).await.unwrap();

        // 已知路径即使内容和大小变化仍跳过哈希, 数据库路径也不删除重插.
        std::fs::write(root.join("first.mkv"), b"changed content with a different size").unwrap();
        let unchanged = collect_files(&root, repository::list_paths(&pool).await.unwrap()).unwrap();
        assert_eq!(unchanged.count, 2);
        assert!(unchanged.added.is_empty());
        assert!(unchanged.removed.is_empty());
        repository::sync_scanned_files(&pool, &unchanged.added, &unchanged.removed).await.unwrap();
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT rowid FROM file_path WHERE path = ''")
            .fetch_one(&pool).await.unwrap(), rowid);
        assert_eq!(repository::get_video(&pool, &id).await.unwrap().unwrap().size, 8);

        std::fs::remove_file(root.join("first.mkv")).unwrap();
        std::fs::rename(root.join("series/copy.MKV"), root.join("renamed.mp4")).unwrap();
        std::fs::write(root.join("new.mkv"), b"new video").unwrap();
        let moved = collect_files(&root, repository::list_paths(&pool).await.unwrap()).unwrap();
        assert_eq!(moved.count, 2);
        assert_eq!(moved.added.len(), 2);
        assert_eq!(moved.removed.len(), 2);
        repository::sync_scanned_files(&pool, &moved.added, &moved.removed).await.unwrap();
        let video = repository::get_video(&pool, &id).await.unwrap().unwrap();
        assert_eq!(video.name, "renamed.mp4");
        assert_eq!(video.watch_pos, Some(12.5));
        assert_eq!(video.ddp_epi_id, Some(1));
        assert_eq!(video.audio_stream_index, Some(1));
        assert_eq!(video.subtitle_stream_index, Some(2));

        std::fs::remove_file(root.join("renamed.mp4")).unwrap();
        let removed = collect_files(&root, repository::list_paths(&pool).await.unwrap()).unwrap();
        assert_eq!(removed.count, 1);
        assert!(removed.added.is_empty());
        repository::sync_scanned_files(&pool, &removed.added, &removed.removed).await.unwrap();
        assert!(repository::get_paths(&pool, &id).await.unwrap().is_empty());
        assert_eq!(repository::get_progress(&pool, &id).await.unwrap().unwrap().watch_pos, Some(12.5));

        // 最后一个副本消失后恢复同内容文件, 仍复用原观看进度和识别信息.
        std::fs::write(root.join("restored.mkv"), b"original").unwrap();
        let restored = collect_files(&root, repository::list_paths(&pool).await.unwrap()).unwrap();
        repository::sync_scanned_files(&pool, &restored.added, &restored.removed).await.unwrap();
        let restored = repository::get_video(&pool, &id).await.unwrap().unwrap();
        assert_eq!(restored.watch_pos, Some(12.5));
        assert_eq!(restored.audio_stream_index, Some(1));
        assert_eq!(restored.subtitle_stream_index, Some(2));
        std::fs::remove_dir_all(&root).unwrap();
        assert!(collect_files(&root, repository::list_paths(&pool).await.unwrap()).is_err());
        assert_eq!(repository::list_paths(&pool).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn failed_incremental_transaction_does_not_delete_existing_paths() {
        // 故意重复插入同一地址触发约束错误, 此前的删除和新文件插入必须一起回滚.
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
        let existing = ScannedFile {
            pre16mib_md5: "a".repeat(32), size: 1, audio_stream_index: Some(1),
            subtitle_stream_index: None, path: String::new(),
            name_no_ext: "old".to_string(), extension: "mkv".to_string(),
        };
        repository::sync_scanned_files(&pool, &[existing], &[]).await.unwrap();
        let removed = repository::list_paths(&pool).await.unwrap();
        let added = (0..2).map(|_| ScannedFile {
            pre16mib_md5: "b".repeat(32), size: 2, audio_stream_index: None,
            subtitle_stream_index: None, path: String::new(),
            name_no_ext: "new".to_string(), extension: "mkv".to_string(),
        }).collect::<Vec<_>>();
        assert!(repository::sync_scanned_files(&pool, &added, &removed).await.is_err());
        let paths = repository::list_paths(&pool).await.unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].name_no_ext, "old");
        assert!(repository::get_progress(&pool, &"b".repeat(32)).await.unwrap().is_none());
    }
}
