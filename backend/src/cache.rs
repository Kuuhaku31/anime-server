
// backend/src/cache.rs
//! 管理缓存根目录与全部缓存实例句柄的一一对应关系.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::{config::AppConfigManager, constants::CACHE_VERSION, models::{CacheSpec, MediaMetadata}, process::{self, ProcessManager, WorkerResources}, utils};

mod handle;

#[cfg(test)]
#[path = "tests/cache.rs"]
mod tests;

use handle::CacheInstanceHandle;

/// 返回稳定缓存身份和公开状态, 不暴露内部目录.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CacheSnapshot {

    /// 客户端用来查询和播放的稳定句柄标识.
    pub(crate) handle_id: String,
    /// 已确认的媒体内容和流选择.
    pub(crate) spec: CacheSpec,
    /// 当前生成进度.
    pub(crate) status: CacheStatus,
    /// 当前转码 worker 占用.
    pub(crate) resources: WorkerResources,
    /// 稳定排序后的首个现存源路径, 无源缓存为 None.
    pub(crate) source_path: Option<PathBuf>,
}

/// 描述缓存状态和已发布范围, 不保存播放位置.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CacheStatus {
    /// 当前缓存阶段.
    pub(crate) phase: String,
    /// 已验证并发布的连续前缀终点秒数.
    pub(crate) buffered_until: f64,
    /// 最近一次恢复或转码失败的诊断, 正常状态为 None.
    pub(crate) error: Option<String>,
}

/// 区分句柄不存在和正在停止, 供 API 显式映射 HTTP 状态.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CacheError {
    /// 句柄不存在或已经封闭.
    NotFound,
    /// 句柄正在停止, 不能接受新的构建请求.
    Stopping,
}

impl std::fmt::Display for CacheError {
    /// 提供稳定可读的缓存操作诊断.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "cache handle not found",
            Self::Stopping => "cache generation is stopping",
        })
    }
}

impl std::error::Error for CacheError {}

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
    pub(crate) async fn build(
        &self, spec: CacheSpec, source_path: PathBuf, metadata: MediaMetadata,
        config: &AppConfigManager, processes: &ProcessManager,
    ) -> Result<CacheSnapshot> {
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

    /// 复制句柄后释放全局锁, 按稳定标识顺序读取快照.
    pub(crate) async fn list(&self) -> Result<Vec<CacheSnapshot>> {
        let mut handles: Vec<_> = self.handles.lock().await.values().cloned().collect();
        handles.sort_by(|left, right| left.handle_id.cmp(&right.handle_id));
        let mut snapshots = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.snapshot().await {
                Ok(snapshot) => snapshots.push(snapshot),
                // 已复制的句柄可能被并发删除, 不把正常删除视为整个列表故障.
                Err(error) if error.downcast_ref::<CacheError>() == Some(&CacheError::NotFound) => {},
                Err(error) => return Err(error),
            }
        }
        Ok(snapshots)
    }

    /// 按稳定标识查询缓存状态, 不启动恢复或生成.
    pub(crate) async fn status(&self, handle_id: &str) -> Result<CacheSnapshot> {
        self.handle(handle_id).await?.snapshot().await
    }

    /// 返回实际流选择, 供重试和源文件查询使用.
    pub(crate) async fn spec(&self, handle_id: &str) -> Result<CacheSpec> {
        Ok(self.handle(handle_id).await?.spec.clone())
    }

    /// 只返回正式清单中已经发布的媒体资源路径.
    pub(crate) async fn media_path(&self, handle_id: &str, asset_name: &str) -> Result<PathBuf> {
        self.handle(handle_id).await?.media_path(asset_name).await
    }

    /// 只重试现有句柄, 不在并发删除后隐式新建缓存目录.
    pub(crate) async fn retry(
        &self, handle_id: &str, source_path: PathBuf, metadata: MediaMetadata,
        config: &AppConfigManager, processes: &ProcessManager,
    ) -> Result<CacheSnapshot> {
        let handle = self.handle(handle_id).await?;
        handle.load(processes).await?;
        handle.run(source_path, metadata, config, processes).await?;
        handle.snapshot().await
    }

    /// 停止当前生成并保留前缀, 随后可以显式重试续接.
    pub(crate) async fn stop(&self, handle_id: &str) -> Result<CacheSnapshot> {
        let handle = self.handle(handle_id).await?;
        handle.pause().await?;
        handle.snapshot().await
    }

    /// 停止实例, 删除实际目录后才从句柄表移除.
    pub(crate) async fn remove(&self, handle_id: &str) -> Result<()> {
        check_handle_id(handle_id)?;
        let mut handles = self.handles.lock().await;
        let handle = handles.get(handle_id).ok_or(CacheError::NotFound)?;
        handle.end().await?;
        handles.remove(handle_id);
        Ok(())
    }

    /// 停止并回收所有写入任务, 保留全部缓存实例和已发布前缀.
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let handles: Vec<_> = self.handles.lock().await.values().cloned().collect();
        let mut errors = Vec::new();
        for handle in handles {
            if let Err(error) = handle.stop().await {
                errors.push(format!("{}: {error:#}", handle.handle_id));
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
            let spec = descriptor.with_context(|| format!(
                "cannot load cache instance {}; files preserved, repair or remove it manually", path.display()))?;
            handles.insert(name.clone(), Arc::new(CacheInstanceHandle::new(spec, name, path)));
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

    /// 校验稳定标识后从登记表读取句柄, 不拼接客户端路径.
    async fn handle(&self, handle_id: &str) -> Result<Arc<CacheInstanceHandle>> {
        check_handle_id(handle_id)?;
        self.handles.lock().await.get(handle_id).cloned().ok_or_else(|| CacheError::NotFound.into())
    }
}

/// 只接受规范的小写内容哈希, 拒绝路径片段和大小写别名.
pub(crate) fn check_handle_id(handle_id: &str) -> Result<()> {
    ensure!(utils::check_md5_ok(handle_id) && handle_id == handle_id.to_ascii_lowercase(),
        "handle_id must be 32 lowercase hexadecimal characters");
    Ok(())
}
