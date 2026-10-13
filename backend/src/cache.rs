//! 统一管理媒体元数据缓存, 活动播放缓存和磁盘淘汰.

use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File, path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use tokio::{task::JoinSet};
use tokio::sync::{watch, Mutex, Notify};
use tracing::{info, warn};

use sqlx::SqlitePool;

use crate::{cache::repository::{CacheFolder, CacheInner, REVISION}, config::PlaybackConfig, db, utils};

mod disk;
mod metadata;
mod repository;


use disk::{disk_usage, persist_used};
use crate::process::{
    self, CachedMedia, ProcessManager,
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

    pub(crate) async fn new(root: PathBuf, config: PlaybackConfig, database: SqlitePool, processes: ProcessManager) -> Result<Self> {

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
        crate::process::clean_startup(&root).await?; // 清理上次异常退出遗留的临时字幕和字体目录, 避免占用磁盘空间和干扰新任务.
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

        let folder_entries = self.inner.entries.lock().await; // 解锁
        let variant_key = config.variant_key().to_owned();

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
        ).await?.context("video file not found")?;
        let spec = crate::models::EncodeSpec::from_config(config.clone(), source, info)?;
        let mut entries = self.inner.entries.lock().await;
        ensure!(!self.is_stopping(), "server is shutting down");
        ensure!(entries.len() < 32, "too many active playback configurations");
        let output = Arc::new(CacheFolder {
            variant_key: variant_key.clone(),
            directory: self.inner.root.join("playback").join(&variant_key), spec: spec,
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

        ensure!(process::media_filename(asset_name), "playback asset not found");
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
