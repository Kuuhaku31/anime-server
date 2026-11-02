
// backend/src/cache.rs
//! 管理缓存根目录与全部缓存实例句柄的一一对应关系.

use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::warn;

use crate::{config::AppConfigManager, constants::CACHE_VERSION, models::{CacheSpec, MediaMetadata}, process::{self, ProcessManager}, utils};

mod handle;

#[cfg(test)]
#[path = "tests/cache.rs"]
mod tests;

use handle::CacheInstanceHandle;

/// 返回缓存状态, 但不向前端暴露内部目录.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct HandleSnapshot {
    /// SessionManager 内部保存的缓存文件夹地址.
    #[serde(skip)]
    pub(crate) folder: PathBuf,
    /// 当前缓存阶段.
    pub(crate) phase: String,
    /// 已验证并发布的连续前缀终点秒数.
    pub(crate) buffered_until: f64,
}

/// 只持有根目录和全量句柄表, 不拥有数据库, 进程或 worker 集合.
pub(crate) struct CacheManager {
    /// 已规范化的缓存根目录.
    root: PathBuf,
    /// 每个实际实例目录唯一对应的句柄.
    handles: Mutex<HashMap<String, Arc<CacheInstanceHandle>>>,
}

impl CacheManager {
    /// 发现全部合法实例并请求句柄自行恢复, 不启动转码.
    pub(crate) async fn new(root: PathBuf, processes: &ProcessManager) -> Result<Self> {
        tokio::fs::create_dir_all(&root).await?;
        let root = tokio::fs::canonicalize(root).await?;
        let playback = root.join("playback");
        tokio::fs::create_dir_all(&playback).await?;
        ensure!(tokio::fs::symlink_metadata(&playback).await?.is_dir(), "unsafe playback cache root");
        let manager = Self { root, handles: Mutex::new(HashMap::new()) };
        manager.discover_handles().await?;
        let handles: Vec<_> = manager.handles.lock().await.values().cloned().collect();
        for handle in handles { handle.load(processes).await?; }
        Ok(manager)
    }

    /// 查找或创建稳定身份的句柄, 恢复和生成均由句柄自行调度.
    pub(crate) async fn open(
        &self, spec: CacheSpec, source_path: PathBuf, metadata: MediaMetadata,
        config: &AppConfigManager, processes: &ProcessManager,
    ) -> Result<HandleSnapshot> {
        let handle_id = spec.handle_id()?;
        let handle = {
            let mut handles = self.handles.lock().await;
            if let Some(handle) = handles.get(&handle_id) {
                ensure!(handle.spec == spec, "cache identity hash collision");
                handle.clone()
            } else {
                let handle = self.create_handle(spec, handle_id.clone())?;
                handles.insert(handle_id, handle.clone());
                handle
            }
        };
        handle.load(processes).await?;
        handle.run(source_path, metadata, config, processes).await?;
        handle.snapshot().await
    }

    /// 按会话保存的文件夹路径查询缓存状态.
    pub(crate) async fn status(&self, folder: &Path) -> Result<HandleSnapshot> {
        self.handle_by_path(folder).await?.snapshot().await
    }

    /// 只返回正式清单中已经发布的媒体资源路径.
    pub(crate) async fn media_path(&self, folder: &Path, asset_name: &str) -> Result<PathBuf> {
        self.handle_by_path(folder).await?.media_path(asset_name).await
    }

    /// 停止实例, 删除实际目录后才从句柄表移除.
    pub(crate) async fn remove(&self, folder: &Path) -> Result<()> {
        let handle_id = self.path_handle_id(folder)?;
        let mut handles = self.handles.lock().await;
        let handle = handles.get(handle_id).context("cache instance not found")?;
        handle.end().await?;
        handles.remove(handle_id);
        Ok(())
    }

    /// 停止所有写入任务, 保留稳定缓存, 删除经历 Stopping 的未完成实例.
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let handles: Vec<_> = self.handles.lock().await.values().cloned().collect();
        let mut errors = Vec::new();
        for handle in handles {
            match handle.stop().await {
                Ok(true) => {
                    if let Err(error) = self.remove(&handle.path).await {
                        errors.push(format!("{}: {error:#}", handle.handle_id));
                    }
                },
                Ok(false) => {},
                Err(error) => errors.push(format!("{}: {error:#}", handle.handle_id)),
            }
        }
        ensure!(errors.is_empty(), "cache shutdown failed: {}", errors.join("; "));
        Ok(())
    }

    /// 只处理缓存根目录中的直接子目录, 非本程序命名的内容明确阻止启动.
    async fn discover_handles(&self) -> Result<()> {
        let mut entries = tokio::fs::read_dir(self.root.join("playback")).await?;
        let mut handles = self.handles.lock().await;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().into_string().map_err(|_| anyhow::anyhow!("non-UTF-8 cache directory"))?;
            ensure!(utils::check_md5_ok(&name) && name == name.to_ascii_lowercase(),
                "unrecognized playback cache entry: {}", entry.path().display());
            let path = entry.path();
            ensure!(entry.file_type().await?.is_dir(), "unsafe cache instance directory: {}", path.display());
            let descriptor = async {
                let bytes = process::regular_bytes(&path.join("instance.json"), 64 * 1024).await?;
                let spec = CacheSpec::from_descriptor(&bytes)?;
                ensure!(utils::check_md5_ok(&spec.media_id) && spec.media_id == spec.media_id.to_ascii_lowercase()
                    && spec.video_stream_index >= 0 && spec.audio_stream_index.is_none_or(|index| index >= 0)
                    && spec.subtitle_stream_index.is_none_or(|index| index >= 0)
                    && spec.version == CACHE_VERSION && spec.handle_id()? == name,
                    "cache descriptor does not match directory or encoding contract");
                Ok::<_, anyhow::Error>(spec)
            }.await;
            match descriptor {
                Ok(spec) => {
                    handles.insert(name.clone(), Arc::new(CacheInstanceHandle::new(spec, name, path)));
                },
                Err(error) => {
                    // 权限或设备错误不能被当作损坏身份, 防止误删可恢复数据.
                    if error.downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() != std::io::ErrorKind::NotFound) {
                        return Err(error).context("cannot read cache descriptor");
                    }
                    warn!(path = %path.display(), "removing invalid cache instance: {error:#}");
                    tokio::fs::remove_dir_all(&path).await.context("cannot remove invalid cache instance")?;
                },
            }
        }
        Ok(())
    }

    /// 在不可中途取消的短临界区建立目录和身份, 避免请求中断后遗留无句柄目录.
    fn create_handle(&self, spec: CacheSpec, handle_id: String) -> Result<Arc<CacheInstanceHandle>> {
        use std::io::Write;

        let descriptor = spec.descriptor()?;
        let path = self.root.join("playback").join(&handle_id);
        std::fs::create_dir(&path).context("cache directory exists without a registered handle")?;
        let persisted = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path.join("instance.json"))?;
            file.write_all(&descriptor)?;
            file.sync_all()?;
            std::fs::File::open(&path)?.sync_all()?;
            std::fs::File::open(self.root.join("playback"))?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = persisted {
            std::fs::remove_dir_all(&path).context("cannot clean incomplete cache instance")?;
            return Err(error);
        }
        Ok(Arc::new(CacheInstanceHandle::new(spec, handle_id, path)))
    }

    /// 校验文件夹地址后返回唯一句柄, 不依赖源媒体是否仍然存在.
    async fn handle_by_path(&self, folder: &Path) -> Result<Arc<CacheInstanceHandle>> {
        let handle_id = self.path_handle_id(folder)?;
        self.handles.lock().await.get(handle_id).cloned().context("cache instance not found")
    }

    /// 拒绝缓存根目录之外或非实例名称的路径.
    fn path_handle_id<'a>(&self, folder: &'a Path) -> Result<&'a str> {
        if folder.parent() != Some(self.root.join("playback").as_path()) {
            bail!("cache folder is outside playback root");
        }
        let handle_id = folder.file_name().and_then(|name| name.to_str()).context("invalid cache folder")?;
        ensure!(utils::check_md5_ok(handle_id), "invalid cache handle ID");
        Ok(handle_id)
    }
}
