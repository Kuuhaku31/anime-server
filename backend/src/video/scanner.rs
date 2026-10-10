//! 扫描媒体目录并同步文件索引.

use std::{
    fs::File,
    io::Read,
    path::Path,
};

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use walkdir::WalkDir;

use crate::{
    config::VIDEO_ROOT,
    db::{models::ScannedFile, repository},
    AppState,
};

const EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
const HASH_LIMIT: u64 = 16 * 1024 * 1024;

/// 手动和定时扫描共用互斥锁, 只有完整收集成功后才能替换数据库路径快照.
pub async fn scan_once(state: &AppState) -> Result<u64> {
    let _scan = state.scan_lock.lock().await;
    // 遍历和文件哈希使用同步 I/O, 放到阻塞线程池以免拖住 HTTP 请求和播放心跳.
    let files = tokio::task::spawn_blocking(collect_files).await??;
    repository::sync_scanned_files(&state.db, &files).await?;
    Ok(files.len() as u64)
}

fn collect_files() -> Result<Vec<ScannedFile>> {
    let root = Path::new(VIDEO_ROOT);
    if !root.exists() {
        anyhow::bail!("media directory is missing");
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry.context("failed to read media directory entry")?;
        let absolute_path = entry.path();
        if !entry.file_type().is_file() || !is_video(absolute_path) {
            continue;
        }
        files.push(scan_file(root, absolute_path)?);
    }

    Ok(files)
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
        path,
        name_no_ext,
        extension,
    })
}

// 最多读取前 16 MiB, 短文件读到 EOF, 文件名或目录变化不会改变业务身份.
fn hash_prefix(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
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
