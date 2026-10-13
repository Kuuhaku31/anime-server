//! 统一管理媒体元数据缓存, 活动播放缓存和磁盘淘汰.

use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File, future::Future, path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Arc, Weak},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use tokio::{process::Child, sync::{watch, Mutex, Notify, OwnedSemaphorePermit}, task::JoinSet, time::interval};
use tracing::{info, warn};

use sqlx::SqlitePool;

use crate::{cache::metadata::MetadataEntry, config::PlaybackConfig, db, utils};

mod disk;
mod metadata;
#[cfg(test)]
mod tests;

use disk::{disk_usage, persist_used};
use crate::video::{
    self, CachedMedia, EncodeSpec, RenderAssets,
    Init, Tail, cancelled, check_cancel, read_tail, Cancelled, Processes,
};
use crate::models::PlaybackVariantConfig;


#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CachePhase {
    Loading,
    Ready,
    Running,
    Stopping,
    Complete,
    Failed,
}

/// SessionManager 所需的最小缓存状态快照.
#[derive(Clone)]
pub(crate) struct CacheSnapshot {
    phase: CachePhase,
    revision: u64,
    buffered_until: f64,
    duration: f64,
}

impl CacheSnapshot {
    pub(crate) fn phase(&self) -> CachePhase { self.phase }
    pub(crate) fn revision(&self) -> u64 { self.revision }
    pub(crate) fn buffered_until(&self) -> f64 { self.buffered_until }
    pub(crate) fn duration(&self) -> f64 { self.duration }
}

enum FolderPhase {
    Loading { cancel: watch::Sender<bool> },
    Ready,
    Running {
        cancel: watch::Sender<bool>,
        desired_threads: usize,
        encoding_threads: usize,
        pause_after: Option<usize>,
        deadline: Option<Instant>,
    },
    Stopping {
        cancel: watch::Sender<bool>,
        loading: bool,
        stop_after: Option<usize>,
        deadline: Option<Instant>,
    },
    Complete,
    Failed,
}

impl FolderPhase {
    fn snapshot(&self) -> CachePhase {
        match self {
            Self::Loading { .. } => CachePhase::Loading,
            Self::Ready => CachePhase::Ready,
            Self::Running { .. } => CachePhase::Running,
            Self::Stopping { .. } => CachePhase::Stopping,
            Self::Complete => CachePhase::Complete,
            Self::Failed => CachePhase::Failed,
        }
    }

    fn cancel(&self) -> Option<watch::Sender<bool>> {
        match self {
            Self::Loading { cancel } | Self::Running { cancel, .. } | Self::Stopping { cancel, .. } => Some(cancel.clone()),
            Self::Ready | Self::Complete | Self::Failed => None,
        }
    }
}

struct FolderState {
    media: CachedMedia,
    phase: FolderPhase,
    revision: u64,
    last_used: SystemTime,
}

/// 缓存统一入口, 独占目录锁并管理两个按需加载的 Map.
///
/// new 校验缓存根目录并初始化临时资源目录和容量维护.
/// media_info 查询或重试成功元数据缓存, acquire 恢复共享缓存并返回窄状态快照.
/// start 校验并保存启动线程数, 但不接收播放等待位置.
/// status 仅返回 SessionManager 所需字段, 不泄露生成控制数据.
/// shutdown 拒绝新请求, 停止生成任务并等待写入者退出.
#[derive(Clone)]
pub(crate) struct CacheManager {
    inner: Arc<CacheInner>,
}

impl CacheManager {

    pub(crate) async fn new(root: PathBuf, config: PlaybackConfig, database: SqlitePool, processes: Processes) -> Result<Self> {

        info!("开始初始化缓存管理器, 检查配置和磁盘目录...");

        config.validate()?; // 启动时检查配置合法性, 避免运行中出现无法恢复的死锁或资源耗尽.
        // tokio::fs::create_dir_all(&root).await?; // 确保缓存目录存在, 但不检查其内容, 由 recover_disk 修复.
        // let root = tokio::fs::canonicalize(root).await?; // 规范化路径, 避免符号链接绕过安全检查, 也避免不同路径引用同一目录导致重复扫描和缓存.
        let directory_lock = File::open(&root)?;
        directory_lock.try_lock().context("cache directory is already in use by another server")?;

        // 检查缓存目录的写权限, 避免启动后才发现无法写入.
        for directory_name in ["playback", "subtitles", "fonts"] {
            let directory = root.join(directory_name);
            tokio::fs::create_dir_all(&directory).await?;
            ensure!(tokio::fs::symlink_metadata(&directory).await?.is_dir(), "unsafe cache directory: {directory_name}");
            let probe = directory.join(format!(".write-{}", utils::random_id()?));
            tokio::fs::OpenOptions::new().write(true).create_new(true).open(&probe).await
                .with_context(|| format!("cache directory is not writable: {}", directory.display()))?;
            tokio::fs::remove_file(probe).await?;
        }
        crate::video::clean_startup(&root).await?; // 清理上次异常退出遗留的临时字幕和字体目录, 避免占用磁盘空间和干扰新任务.
        info!("缓存管理器配置和磁盘目录检查通过, 开始恢复磁盘缓存和启动后台维护任务...");

        let (stop, _) = watch::channel(false);
        let manager = Self {
            inner: Arc::new(CacheInner {
                root, config, database, processes, metadata: Mutex::new(HashMap::new()),
                entries: Mutex::new(HashMap::new()),
                folders: Mutex::new(HashMap::new()),
                workers: Mutex::new(JoinSet::new()), maintenance: Mutex::new(()), stop,
                _directory_lock: directory_lock,
            }),
        };
        manager.maintain().await?;
        info!("缓存管理器初始化完成, 根目录: {}, 最大缓存容量: {} MiB, 最大并发编码线程数: {}",
            manager.inner.root.display(), manager.inner.config.cache_max_bytes / 1024 / 1024,
            manager.inner.config.max_total_ffmpeg_threads);
        Ok(manager)
    }

    /// 恢复并激活共享缓存, 不启动新的 FFmpeg 编码任务.
    ///
    /// 根据 PlaybackVariantConfig 找到或创建一个共享缓存实例,
    /// 确保该缓存的磁盘状态已经恢复并可以使用, 然后返回 CacheSnapshot
    pub(crate) async fn acquire(&self, config: &PlaybackVariantConfig) -> Result<CacheSnapshot> {

        let variant_key = config.variant_key().to_owned();

        let folder_entries = self.inner.entries.lock().await; // 解锁

        // 检查是否已有对应的缓存实例
        if let Some(folder) = folder_entries.get(&variant_key).cloned() {

            let mut folder_state = folder.state.lock().await;
            ensure!(folder.spec.config == *config, "playback configuration hash collision");

            let wait = match &folder_state.phase {
                FolderPhase::Stopping { .. } => bail!("playback cache is stopping"),
                FolderPhase::Failed => {
                    folder_state.phase = FolderPhase::Loading { cancel: watch::channel(false).0 };
                    folder_state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
                    folder_state.last_used = SystemTime::now();
                    true
                },
                _ => {
                    folder_state.last_used = SystemTime::now();
                    if !matches!(folder_state.phase, FolderPhase::Loading { .. }) {
                        return Ok(folder.snapshot(&folder_state));
                    }
                    false
                },
            };
            drop(folder_state);
            drop(folder_entries);
            if wait { self.spawn_load(&folder).await; }
            return folder.wait_loaded().await;
        }

        // 缓存内没有对应文件夹
        ensure!(folder_entries.len() < 32, "too many active playback configurations");
        drop(folder_entries);
        let info = self.media_info(config.src_file_pre16mib_md5()).await.map_err(|e| anyhow::anyhow!(e))?;
        let source = db::get_a_video_path_by_file_id(
            &self.inner.database, config.src_file_pre16mib_md5(),
        ).await?
            .context("video file not found")?;
        let spec = EncodeSpec::from_config(config.clone(), source, info)?;
        let mut entries = self.inner.entries.lock().await;
        ensure!(!self.is_stopping(), "server is shutting down");
        ensure!(entries.len() < 32, "too many active playback configurations");
        let output = Arc::new(CacheFolder {
            variant_key: variant_key.clone(),
            directory: self.inner.root.join("playback").join(&variant_key), spec: spec.clone(),
            state: Mutex::new(FolderState {
                media: CachedMedia::default(),
                phase: FolderPhase::Loading { cancel: watch::channel(false).0 },
                revision: REVISION.fetch_add(1, Ordering::Relaxed), last_used: SystemTime::now(),
            }),
            changed: Notify::new(),
        });
        // 先保留唯一活动元素, 磁盘校验在该元素的任务内进行, 不阻塞其他会话的 DELETE.
        match entries.entry(variant_key.clone()) {
            Entry::Vacant(entry) => { entry.insert(output.clone()); },
            Entry::Occupied(_) => bail!("playback cache was activated by another request"),
        }
        self.inner.folders.lock().await.insert(variant_key.clone(), Arc::downgrade(&output));
        drop(entries);
        self.spawn_load(&output).await;
        output.wait_loaded().await
    }

    pub(crate) async fn status(&self, variant_key: &str) -> Result<CacheSnapshot> {
        let output = self.get(variant_key).await?;
        let mut state = output.state.lock().await;
        state.last_used = SystemTime::now();
        Ok(output.snapshot(&state))
    }

    /// 返回 SessionManager 选择启动线程数所需的基础值和变体上限.
    pub(crate) fn thread_limits(&self) -> (usize, usize) {
        (self.inner.config.ffmpeg_threads, self.inner.config.max_variant_threads)
    }

    /// 请求启动未完成缓存; 编码启动前的并发请求可以覆盖期望线程数.
    pub(crate) async fn start(&self, variant_key: &str, desired_threads: usize) -> Result<()> {
        let (base, maximum) = self.thread_limits();
        ensure!(desired_threads >= base && desired_threads <= maximum,
            "requested encoder threads must be between {base} and {maximum}");
        ensure!(!self.is_stopping(), "server is shutting down");
        loop {
            let output = self.get(variant_key).await?;
            let notified = output.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut state = output.state.lock().await;
            state.last_used = SystemTime::now();
            let spawn = match &mut state.phase {
                FolderPhase::Ready => {
                    let (cancel, _) = watch::channel(false);
                    state.phase = FolderPhase::Running {
                        cancel, desired_threads, encoding_threads: 0, pause_after: None, deadline: None,
                    };
                    state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
                    true
                },
                FolderPhase::Running { cancel, desired_threads: requested, encoding_threads, .. } => {
                    if *cancel.borrow() { drop(state); notified.await; continue; }
                    if *encoding_threads == 0 { *requested = desired_threads; }
                    false
                },
                FolderPhase::Complete => return Ok(()),
                FolderPhase::Loading { .. } => bail!("cache has not finished loading"),
                FolderPhase::Stopping { .. } => bail!("cache is stopping"),
                FolderPhase::Failed => bail!("cache generation failed"),
            };
            drop(state);
            if spawn {
                self.spawn_generation(&output).await;
                output.changed.notify_waiters();
            }
            return Ok(());
        }
    }

    /// SessionManager 可请求暂停, 但暂停策略仍由缓存任务在安全边界执行.
    #[allow(dead_code)]
    pub(crate) async fn pause(&self, variant_key: &str) -> Result<()> {
        let output = self.get(variant_key).await?;
        let mut state = output.state.lock().await;
        let next_segment = state.media.segments.len() + 1;
        if let FolderPhase::Running { cancel, encoding_threads, pause_after, deadline, .. } = &mut state.phase {
            if *encoding_threads == 0 {
                cancel.send_replace(true);
            } else {
                *pause_after = Some(next_segment);
                *deadline = Some(Instant::now() + Duration::from_secs(30));
            }
        }
        Ok(())
    }

    /// 最后一个会话离开时释放缓存 Map 条目或请求 worker 安全停止.
    pub(crate) async fn release(&self, variant_key: &str) -> Result<()> {
        let mut entries = self.inner.entries.lock().await;
        let Some(output) = entries.get(variant_key).cloned() else { return Ok(()); };
        let mut state = output.state.lock().await;
        state.last_used = SystemTime::now();
        match &state.phase {
            FolderPhase::Loading { cancel } => {
                state.phase = FolderPhase::Stopping {
                    cancel: cancel.clone(), loading: true, stop_after: None, deadline: None,
                };
            },
            FolderPhase::Running { cancel, encoding_threads, .. } => {
                let cancel = cancel.clone();
                let encoding = *encoding_threads > 0;
                let (stop_after, deadline) = if encoding {
                    (Some(state.media.segments.len() + 1), Some(Instant::now() + Duration::from_secs(30)))
                } else { (None, None) };
                state.phase = FolderPhase::Stopping { cancel: cancel.clone(), loading: false, stop_after, deadline };
                if !encoding { cancel.send_replace(true); }
            },
            FolderPhase::Stopping { .. } => return Ok(()),
            FolderPhase::Ready | FolderPhase::Complete | FolderPhase::Failed => {
                drop(state);
                entries.remove(variant_key);
                drop(entries);
                output.changed.notify_waiters();
                persist_used(&output.directory, SystemTime::now()).await?;
                return Ok(());
            },
        }
        drop(state);
        drop(entries);
        output.changed.notify_waiters();
        Ok(())
    }

    /// 获取分片资源.
    /// 只开放正式清单中的已发布资源, 私有工作目录和未验证尾部不能被读取.
    pub(crate) async fn media_path(&self, variant_key: &str, asset_name: &str) -> Result<PathBuf> {

        ensure!(video::media_filename(asset_name), "playback asset not found");
        let output = self.get(variant_key).await?;
        let mut state = output.state.lock().await;
        ensure!(!matches!(&state.phase, FolderPhase::Stopping { .. }), "playback cache is stopping");
        ensure!(!state.media.segments.is_empty(), "playback output is not ready");
        if let Some(index) = asset_name.strip_prefix("segment_")
            .and_then(|asset_name| asset_name.strip_suffix(".m4s")) {
            ensure!(index.parse::<usize>()? < state.media.segments.len(), "playback segment is not published");
        }
        state.last_used = SystemTime::now();
        let path = output.directory.join(asset_name);
        let file = tokio::fs::symlink_metadata(&path).await.context("cached media file is missing")?;
        ensure!(file.is_file() && file.len() > 0, "unsafe or empty cached media file");

        Ok(path)
    }

    /// 按磁盘访问时间淘汰非活动缓存, 尚未退出的缓存任务引用仍受保护.
    pub(crate) async fn maintain(&self) -> Result<()> {
        if let Ok(mut workers) = self.inner.workers.try_lock() {
            while let Some(result) = workers.try_join_next() {
                if let Err(error) = result { warn!("cache worker failed: {error}"); }
            }
        }
        let _maintenance = self.inner.maintenance.lock().await;
        self.inner.folders.lock().await.retain(|_, folder| folder.strong_count() > 0);
        let root = self.inner.root.clone();
        let (mut total, mut candidates) = tokio::task::spawn_blocking(move || disk_usage(&root)).await??;
        candidates.sort_by_key(|candidate| candidate.0);
        for (_, variant_key, size) in candidates {
            if total <= self.inner.config.cache_max_bytes { break; }
            let entries = self.inner.entries.lock().await;
            if entries.contains_key(&variant_key) {
                continue;
            }
            let mut folders = self.inner.folders.lock().await;
            if folders.get(&variant_key).is_some_and(|folder| folder.strong_count() > 0) {
                continue;
            }
            let path = self.inner.root.join("playback").join(&variant_key);
            let trash = self.inner.root.join("playback")
                .join(format!(".evict-{variant_key}-{}", utils::random_id()?));
            // 先原子移走不活跃目录, 避免长时间删除文件阻塞其他缓存的状态和释放请求.
            tokio::fs::rename(&path, &trash).await?;
            folders.remove(&variant_key);
            drop(folders);
            drop(entries);
            tokio::fs::remove_dir_all(trash).await?;
            total = total.saturating_sub(size);
            info!(variant_key, "evicted inactive playback cache");
        }
        Ok(())
    }

    /// 先拒绝新租约并请求边界停止, 等全部写入者退出后才关闭媒体进程许可.
    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.inner.stop.send_replace(true);
        {
            let entries = self.inner.entries.lock().await;
            for output in entries.values() {
                let mut state = output.state.lock().await;
                match &state.phase {
                    FolderPhase::Loading { cancel } => {
                        state.phase = FolderPhase::Stopping {
                            cancel: cancel.clone(), loading: true, stop_after: None, deadline: None,
                        };
                    },
                    FolderPhase::Running { cancel, encoding_threads, .. } => {
                        let cancel = cancel.clone();
                        let encoding = *encoding_threads > 0;
                        let (stop_after, deadline) = if encoding {
                            (Some(state.media.segments.len() + 1), Some(Instant::now() + Duration::from_secs(30)))
                        } else { (None, None) };
                        state.phase = FolderPhase::Stopping { cancel: cancel.clone(), loading: false, stop_after, deadline };
                        if !encoding { cancel.send_replace(true); }
                    },
                    _ => {},
                }
            }
        }
        let mut errors = Vec::new();
        {
            let mut workers = self.inner.workers.lock().await;
            while let Some(result) = workers.join_next().await {
                if let Err(error) = result { errors.push(error.to_string()); }
            }
        }
        self.inner.entries.lock().await.clear();
        self.inner.processes.slots.close();
        self.inner.processes.encoders.close();
        if let Err(error) = self.maintain().await { errors.push(format!("{error:#}")); }
        ensure!(errors.is_empty(), "cache shutdown failed: {}", errors.join("; "));
        Ok(())
    }

    pub(crate) fn is_stopping(&self) -> bool { *self.inner.stop.borrow() }
}


// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

impl CacheManager {
    async fn spawn_load(&self, output: &Arc<CacheFolder>) {
        let manager = self.clone();
        let output = output.clone();
        let processes = self.inner.processes.clone();
        self.inner.workers.lock().await.spawn(async move {
            manager.load(&processes, &output).await;
            drop(output);
            if let Err(error) = manager.maintain().await { warn!("cache maintenance failed: {error:#}"); }
        });
    }

    async fn spawn_generation(&self, output: &Arc<CacheFolder>) {
        let manager = self.clone();
        let output = output.clone();
        let processes = self.inner.processes.clone();
        self.inner.workers.lock().await.spawn(async move {
            manager.generate(&processes, &output).await;
            drop(output);
            if let Err(error) = manager.maintain().await { warn!("cache maintenance failed: {error:#}"); }
        });
    }

    async fn get(&self, variant_key: &str) -> Result<Arc<CacheFolder>> {
        self.inner.entries.lock().await.get(variant_key).cloned().context("playback cache is no longer active")
    }

    async fn load(&self, processes: &Processes, output: &Arc<CacheFolder>) {
        let mut cancel = {
            let state = output.state.lock().await;
            let Some(cancel) = state.phase.cancel() else { return; };
            cancel.subscribe()
        };
        let result = self.recover(processes, output, &mut cancel).await;
        let mut entries = self.inner.entries.lock().await;
        let mut state = output.state.lock().await;
        let stopping = matches!(&state.phase, FolderPhase::Stopping { loading: true, .. });
        if stopping || result.is_err() && *cancel.borrow() {
            drop(state);
            if entries.get(&output.variant_key).is_some_and(|current| Arc::ptr_eq(current, output)) {
                entries.remove(&output.variant_key);
            }
        } else {
            match result {
                Ok(media) => {
                    state.media = media;
                    state.phase = if state.media.complete { FolderPhase::Complete } else { FolderPhase::Ready };
                },
                Err(error) => {
                    warn!(variant_key = output.variant_key, "cache recovery failed: {error:#}");
                    state.phase = FolderPhase::Failed;
                },
            }
            state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
            drop(state);
        }
        drop(entries);
        output.changed.notify_waiters();
    }

    async fn generate(&self, processes: &Processes, output: &Arc<CacheFolder>) {
        let mut cancel = {
            let state = output.state.lock().await;
            let Some(cancel) = state.phase.cancel() else { return; };
            cancel.subscribe()
        };
        let result = self.run_generation(processes, output, &mut cancel).await;
        let mut entries = self.inner.entries.lock().await;
        let mut state = output.state.lock().await;
        let stopping = matches!(&state.phase, FolderPhase::Stopping { .. });
        if stopping {
            drop(state);
            if entries.get(&output.variant_key).is_some_and(|current| Arc::ptr_eq(current, output)) {
                entries.remove(&output.variant_key);
            }
        } else if matches!(&state.phase, FolderPhase::Running { .. }) {
            match result {
                Ok(()) => {
                    if !state.media.complete { state.phase = FolderPhase::Ready; }
                },
                Err(error) if error.is::<Cancelled>() && *cancel.borrow() => {
                    state.phase = FolderPhase::Ready;
                },
                Err(error) => {
                    warn!(variant_key = output.variant_key, "cache generation failed: {error:#}");
                    state.phase = FolderPhase::Failed;
                },
            }
            state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
            drop(state);
        }
        drop(entries);
        if let Err(error) = persist_used(&output.directory, SystemTime::now()).await {
            warn!(variant_key = output.variant_key, "cache timestamp update failed: {error:#}");
        }
        output.changed.notify_waiters();
    }

    // 恢复并校验已有磁盘前缀, 返回可继续编码的媒体状态.
    async fn recover(&self, processes: &Processes, output: &Arc<CacheFolder>, cancel: &mut watch::Receiver<bool>) -> Result<CachedMedia> {
        match tokio::fs::create_dir(&output.directory).await {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                ensure!(tokio::fs::symlink_metadata(&output.directory).await?.is_dir(), "unsafe cache directory"),
            Err(error) => return Err(error.into()),
        }
        let media = video::recover(&output.directory, processes, cancel).await?;
        check_cancel(cancel)?;
        Ok(media)
    }

    async fn run_generation(&self, processes: &Processes, output: &Arc<CacheFolder>, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let (resume, complete) = {
            let state = output.state.lock().await;
            (state.media.tail(), state.media.complete)
        };
        if complete { return Ok(()); }
        check_cancel(cancel)?;
        if resume.video > 0 && !video::has_video_after(&output.spec, resume, processes, cancel).await? {
            return self.complete(output).await;
        }
        let directory = output.directory.join(format!(".work-{}", utils::random_id()?));
        let assets = if output.spec.subtitle.as_ref().is_some_and(|track| !track.is_bitmap()) {
            Some(RenderAssets::allocate(&self.inner.root).await?)
        } else { None };
        let created = tokio::fs::create_dir(&directory).await;
        let result = match created {
            Ok(()) => self.render(processes, output, &directory, assets.as_ref(), resume, cancel).await,
            Err(error) => {
                if let Some(assets) = assets { assets.cleanup().await?; }
                return Err(error.into());
            },
        };
        let mut errors = Vec::new();
        if let Some(assets) = assets {
            if let Err(error) = assets.cleanup().await { errors.push(format!("{error:#}")); }
        }
        if let Err(error) = tokio::fs::remove_dir_all(&directory).await { errors.push(error.to_string()); }
        if !errors.is_empty() {
            bail!("{}; temporary render cleanup failed: {}", result.err().map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "render finished".to_string()), errors.join("; "));
        }
        result
    }

    async fn render(&self, processes: &Processes, output: &Arc<CacheFolder>, directory: &Path, assets: Option<&RenderAssets>,
        resume: Tail, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let seconds = self.inner.config.startup_timeout_seconds;
        if let Some(assets) = assets {
            let _permit = tokio::select! {
                biased;
                _ = cancelled(cancel) => return Err(Cancelled.into()),
                permit = processes.slots.acquire() => permit?,
            };
            assets.prepare(&output.spec, seconds, cancel).await?;
        }
        // 先取得基础预算, 真正启动前再读取 SessionManager 最新提交的期望线程数.
        let encoder = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = processes.encoders.clone().acquire_owned() => permit?,
        };
        let base = self.inner.config.ffmpeg_threads;
        let mut threads = processes.encoding_threads(base, base, cancel).await?;
        let process = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = processes.slots.clone().acquire_owned() => permit?,
        };
        let mut child = {
            let mut state = output.state.lock().await;
            check_cancel(cancel)?;
            let FolderPhase::Running { desired_threads, encoding_threads, .. } = &mut state.phase
                else { bail!("cache generation is no longer running"); };
            processes.extend_encoding_threads(&mut threads, *desired_threads)?;
            let child = video::command(&output.spec, directory, assets, resume, threads.num_permits())?
                .spawn().context("failed to start FFmpeg")?;
            *encoding_threads = threads.num_permits();
            child
        };
        info!(variant_key = output.variant_key, pid = child.id(), threads = threads.num_permits(),
            resume = resume.seconds(), "cache generation started");
        let stderr = child.stderr.take().context("FFmpeg stderr is unavailable")?;
        let reader = tokio::spawn(read_tail(stderr));
        let mut budget = Some((encoder, process, threads));
        let result = self.monitor(processes, output, directory, resume, &mut child, &mut budget, cancel).await;
        // 无论取消, 超时还是校验失败, 都先回收精确的子进程, 再释放线程和工作目录.
        if child.try_wait()?.is_none() { child.kill().await?; }
        child.wait().await?;
        drop(budget.take());
        let tail = reader.await.context("FFmpeg error reader failed")??;
        result.with_context(|| if tail.is_empty() { "FFmpeg generation ended".to_string() } else { format!("FFmpeg: {tail}") })
    }

    async fn check_worker(&self, output: &CacheFolder, child: &mut Child,
        budget: &mut EncoderBudget, started: Instant, published: usize, cancel: &watch::Receiver<bool>) -> Result<()> {
        // 发布 future 可能正持锁等待落盘, 监控分支不能再等待同一把锁而停止轮询它.
        let Ok(mut state) = output.state.try_lock() else { return Ok(()); };
        let stop = match &state.phase {
            FolderPhase::Running { deadline, cancel, .. } | FolderPhase::Stopping { deadline, cancel, .. } =>
                Some((*deadline, cancel.clone())),
            _ => None,
        };
        if let Some((Some(deadline), cancel_sender)) = stop {
            if Instant::now() >= deadline {
                cancel_sender.send_replace(true);
                warn!(variant_key = output.variant_key, "forcing cache stop after the 30 second segment deadline");
            }
        }
        check_cancel(cancel)?;
        if let Some(exit) = child.try_wait()? {
            drop(budget.take());
            match &mut state.phase {
                FolderPhase::Running { encoding_threads, .. } => *encoding_threads = 0,
                FolderPhase::Stopping { .. } => {},
                _ => {},
            }
            ensure!(exit.success(), "FFmpeg exited with {exit}");
        }
        ensure!(published > 0 || started.elapsed().as_secs() < self.inner.config.startup_timeout_seconds,
            "FFmpeg startup timed out");
        Ok(())
    }

    // 长耗时校验与编码监控同时推进, 保证等待校验时仍能响应停止和启动超时.
    async fn checked_step<T>(&self, output: &CacheFolder, child: &mut Child, budget: &mut EncoderBudget, started: Instant,
        published: usize, cancel: &watch::Receiver<bool>, step: impl Future<Output = Result<T>>) -> Result<T> {
        let stop = output.state.lock().await.phase.cancel().context("cache worker has no cancellation handle")?;
        tokio::pin!(step);
        let mut tick = interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                result = &mut step => return result,
                _ = tick.tick() => {
                    if let Err(error) = self.check_worker(output, child, budget, started, published, cancel).await {
                        stop.send_replace(true);
                        if child.try_wait()?.is_none() { child.start_kill()?; }
                        // 通知校验子进程自行终止并 wait, 不直接丢弃仍拥有子进程的 future.
                        if let Err(cleanup) = step.await {
                            if !cleanup.is::<Cancelled>() {
                                warn!(variant_key = output.variant_key, "interrupted media step failed: {cleanup:#}");
                            }
                        }
                        return Err(error);
                    }
                },
            }
        }
    }

    // 顺序消费私有清单, 编码进程退出不代表待发布分片已全部处理完.
    async fn monitor(&self, processes: &Processes, output: &CacheFolder, directory: &Path, resume: Tail,
        child: &mut Child, budget: &mut EncoderBudget, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let started = Instant::now();
        let mut published = 0;
        let mut tick = interval(Duration::from_millis(100));
        loop {
            self.check_worker(output, child, budget, started, published, cancel).await?;
            // 先确认进程退出再读最终清单, 不能拿校验前的旧快照误判缺少 ENDLIST.
            let finished = child.try_wait()?.is_some_and(|exit| exit.success());
            let playlist = video::read_playlist(directory).await?;
            if let Some(playlist) = playlist {
                if published < playlist.segments.len() {
                    let mut validation_cancel = cancel.clone();
                    self.checked_step(output, child, budget, started, published, cancel,
                        self.publish(processes, output, directory, &playlist.segments[published], resume,
                            published == 0, &mut validation_cancel)).await?;
                    published += 1;
                }
                if finished && published == playlist.segments.len() {
                    ensure!(playlist.complete, "FFmpeg completed without an HLS end marker");
                    ensure!(published > 0, "FFmpeg completed without a playable segment");
                    return self.complete(output).await;
                }
                let state = output.state.lock().await;
                let stop_after = match &state.phase {
                    FolderPhase::Running { pause_after, .. } => *pause_after,
                    FolderPhase::Stopping { stop_after, .. } => *stop_after,
                    _ => None,
                };
                if stop_after.is_some_and(|count| state.media.segments.len() >= count) {
                    if let FolderPhase::Running { cancel, .. } = &state.phase {
                        cancel.send_replace(true);
                    }
                    return Ok(());
                }
            } else {
                ensure!(!finished, "FFmpeg completed without an HLS playlist");
            }
            tick.tick().await;
        }
    }

    // 新分片必须通过 init 兼容, 时间轴和真实解码校验, 不能覆盖任何已发布字节.
    async fn publish(&self, processes: &Processes, output: &CacheFolder, directory: &Path, source_name: &str, resume: Tail,
        first: bool, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let init_bytes = video::regular_bytes(&directory.join("init.mp4"), 4 * 1024 * 1024).await?;
        let init = Init::parse(&init_bytes)?;
        ensure!(init.audio() == output.spec.config.audio_stream().is_some(), "unexpected generated audio track");
        let (expected, count) = {
            let mut state = output.state.lock().await;
            if let Some(existing) = &state.media.init { existing.compatible(&init)?; }
            else {
                video::atomic_file(&output.directory, "init.mp4", &init_bytes).await?;
                state.media.init = Some(init.clone());
            }
            (state.media.tail(), state.media.segments.len())
        };
        ensure!(count < 1_000_000, "too many cached HLS segments");
        let data = video::regular_bytes(&directory.join(source_name), 256 * 1024 * 1024).await?;
        let (data, fragment) = video::normalize(&data, &init, resume, expected, count as u32 + 1)?;
        video::atomic_file(directory, "ready.m4s", &data).await?;
        // 与正式 init 联合解码, 不能只证明分片配合本次临时 init 能播放.
        video::validate_segment(directory, "../init.mp4", "ready.m4s", init.audio(), first,
            processes, cancel).await?;
        let mut state = output.state.lock().await;
        check_cancel(cancel)?;
        let mut media = state.media.clone();
        media.segments.push(fragment);
        media.playlist()?;
        let path = output.directory.join(video::segment_name(count));
        ensure!(!tokio::fs::try_exists(&path).await?, "refusing to overwrite a published cache segment");
        // 先落盘分片再替换清单, 最后推进内存末端, 不向客户端暴露尚不存在的 URI.
        tokio::fs::rename(directory.join("ready.m4s"), path).await?;
        video::sync_directory(&output.directory).await?;
        video::write_playlist(&output.directory, &media).await?;
        state.media = media;
        // 已提交的原始临时分片不再占用双份空间, 正式分片今后只读.
        tokio::fs::remove_file(directory.join(source_name)).await?;
        Ok(())
    }

    // 先提交 ENDLIST 再写空标记, 中途崩溃可由恢复流程重新确认完整性.
    async fn complete(&self, output: &CacheFolder) -> Result<()> {
        let mut state = output.state.lock().await;
        ensure!(!state.media.segments.is_empty(), "cannot complete an empty playback cache");
        let mut media = state.media.clone();
        media.complete = true;
        video::write_playlist(&output.directory, &media).await?;
        video::atomic_file(&output.directory, "complete", b"").await?;
        state.media = media;
        state.phase = FolderPhase::Complete;
        state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
        output.changed.notify_waiters();
        info!(variant_key = output.variant_key, "playback cache complete");
        Ok(())
    }

}

// 私有共享状态只由缓存管理器和生成任务访问.
struct CacheInner {
    root: PathBuf,
    config: PlaybackConfig,
    database: SqlitePool,
    processes: Processes,
    metadata: Mutex<HashMap<String, MetadataEntry>>,
    // 同时访问集合和元素时先锁 entries 再锁 state, 创建与回收必须保持相同顺序.
    entries: Mutex<HashMap<String, Arc<CacheFolder>>>,
    folders: Mutex<HashMap<String, Weak<CacheFolder>>>,
    workers: Mutex<JoinSet<()>>,
    maintenance: Mutex<()>,
    stop: watch::Sender<bool>,
    _directory_lock: File,
}

/// 同一配置的全部会话共享此元素, 只有其生成任务能发布正式分片.
struct CacheFolder {
    variant_key: String,
    directory: PathBuf,
    spec: EncodeSpec,
    state: Mutex<FolderState>,
    changed: Notify,
}

impl CacheFolder {
    fn snapshot(&self, state: &FolderState) -> CacheSnapshot {
        CacheSnapshot {
            phase: state.phase.snapshot(),
            revision: state.revision,
            buffered_until: state.media.tail().seconds(),
            duration: self.spec.duration,
        }
    }

    async fn wait_loaded(&self) -> Result<CacheSnapshot> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let state = self.state.lock().await;
            if !matches!(&state.phase, FolderPhase::Loading { .. }) {
                return Ok(self.snapshot(&state));
            }
            drop(state);
            notified.await;
        }
    }
}

static REVISION: AtomicU64 = AtomicU64::new(1);

// 进程实际退出时立即归还预算, 不把后续磁盘提交算成正在编码.
type EncoderBudget = Option<(OwnedSemaphorePermit, OwnedSemaphorePermit, OwnedSemaphorePermit)>;
