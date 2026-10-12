//! 统计缓存磁盘占用并持久保存目录访问时间.

use std::{fs::{File, FileTimes}, path::Path, time::SystemTime};

use anyhow::{Context, Result};
use crate::utils;

// 用目录 mtime 保存 LRU 访问时间, 无需额外元数据文件且重启后仍可排序.
pub(super) async fn persist_used(directory: &Path, time: SystemTime) -> Result<()> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = File::open(directory)?;
        file.set_times(FileTimes::new().set_modified(time))?;
        file.sync_all()
    }).await.context("cache timestamp update task failed")??;
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size = 0_u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.io_error().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => continue,
            Err(error) => return Err(error.into()),
        };
        if entry.file_type().is_file() {
            match entry.metadata() {
                Ok(metadata) => size = size.checked_add(metadata.len()).context("cache size overflow")?,
                Err(error) if error.io_error().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {},
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(size)
}

// 历史磁盘缓存只参与容量统计, 不在启动时加入活动文件夹 Map.
pub(super) fn disk_usage(root: &Path) -> Result<(u64, Vec<(SystemTime, String, u64)>)> {
    let total = directory_size(root)?;
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(root.join("playback"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if utils::check_md5(&name).is_ok() && entry.file_type()?.is_dir() {
            candidates.push((entry.metadata()?.modified()?, name, directory_size(&entry.path())?));
        }
    }
    Ok((total, candidates))
}
