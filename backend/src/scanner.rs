
// backend/src/scanner.rs
//! 串行扫描媒体目录并同步内容身份与路径关联.

use std::{collections::{HashMap, HashSet}, path::{Component, Path, PathBuf}};

use anyhow::{ensure, Context, Result};
use tokio::sync::Mutex;
use walkdir::WalkDir;

use crate::{constants::EXTENSIONS, db::DatabaseManager, models::{ScanChanges, ScannedFile}};

#[cfg(test)]
#[path = "tests/scanner.rs"]
mod tests;

/// 保存媒体根目录和扫描锁, 不持有数据库管理器.
pub(crate) struct ScannerManager {

    /// 已规范化的媒体根目录.
    root: PathBuf,
    /// 串行化全量扫描和播放前的单路径重扫.
    lock: Mutex<()>,
}

impl ScannerManager {

    /// 校验媒体根目录并创建独立扫描器.
    pub(crate) async fn new(root: &Path) -> Result<Self> {
        let root = tokio::fs::canonicalize(root).await.context("cannot resolve media root")?;
        ensure!(tokio::fs::metadata(&root).await?.is_dir(), "media root is not a directory");
        Ok(Self { root, lock: Mutex::new(()) })
    }

    /// 全量重算内容身份, 只删除本次确实未发现的已登记路径.
    pub(crate) async fn scan_all(&self, database: &DatabaseManager) -> Result<ScanChanges> {
        let _guard = self.lock.lock().await;
        let known: HashMap<_, _> = database.list_paths().await?.into_iter()
            .map(|record| (PathBuf::from(record.0), record.1)).collect();
        let root = self.root.clone();
        let paths = tokio::task::spawn_blocking(move || collect_paths(&root)).await??;
        let mut found = HashSet::new();
        let mut changes = ScanChanges::default();
        let mut replacements = Vec::new();
        let mut failures = Vec::new();
        for path in paths {
            found.insert(path.clone());
            let hash_path = self.resolve(&path).await?;
            let file = tokio::task::spawn_blocking(move || ScannedFile::new(&hash_path)).await?;
            let mut file = match file {
                Ok(file) => file,
                Err(error) => {
                    failures.push(format!("{}: {error:#}", path.display()));
                    continue;
                },
            };
            file.path = path.clone();
            if known.get(&path) == Some(&file.media_id) { continue; }
            replacements.push(file);
            if known.contains_key(&path) { changes.updated += 1; }
            else { changes.added += 1; }
        }
        let removed: Vec<_> = known.keys().filter(|path| !found.contains(*path)).cloned().collect();
        database.replace_scanned_paths(&replacements, &removed).await?;
        changes.removed = removed.len() as u64;
        changes.total = database.list_paths().await?.len() as u64;
        ensure!(failures.is_empty(), "some media files could not be scanned: {}", failures.join("; "));
        Ok(changes)
    }

    /// 播放或手动重扫前重新计算内容身份, 文件消失时删除原路径关联.
    pub(crate) async fn scan_one(&self, database: &DatabaseManager, path: &Path) -> Result<Option<ScannedFile>> {
        let _guard = self.lock.lock().await;
        self.check_path(path)?;
        let resolved = match self.resolve(path).await {
            Ok(resolved) => resolved,
            Err(error) if error.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {
                database.delete_scanned_paths(&[path.to_owned()]).await?;
                return Ok(None);
            },
            Err(error) => return Err(error),
        };
        let mut file = tokio::task::spawn_blocking(move || ScannedFile::new(&resolved)).await??;
        file.path = path.to_owned();
        database.replace_scanned_path(&file).await?;
        Ok(Some(file))
    }

    /// 验证现存文件实际位于媒体目录内, 保留列表展示的路径身份.
    pub(crate) async fn resolve(&self, path: &Path) -> Result<PathBuf> {
        self.check_path(path)?;
        let resolved = tokio::fs::canonicalize(path).await?;
        ensure!(resolved.starts_with(&self.root) && tokio::fs::metadata(&resolved).await?.is_file(),
            "media path must resolve to a file inside media root: {}", path.display());
        Ok(resolved)
    }

    /// 检查请求地址没有越界或父目录跳转, 不读取目标文件.
    fn check_path(&self, path: &Path) -> Result<()> {
        ensure!(path.is_absolute() && path.starts_with(&self.root) && path != self.root
            && path.to_str().is_some() && is_video(path)
            && !path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)),
            "invalid media path: {}", path.display());
        Ok(())
    }
}

/// 在阻塞线程中遍历实际文件, 不跟随媒体根目录内的符号链接.
fn collect_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let entry = entry?;
        if entry.file_type().is_file() && is_video(entry.path()) {
            paths.push(entry.path().to_owned());
        }
    }
    Ok(paths)
}

/// 按不区分大小写的扩展名白名单识别媒体文件.
fn is_video(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str())
        .is_some_and(|value| EXTENSIONS.iter().any(|extension| value.eq_ignore_ascii_case(extension)))
}
