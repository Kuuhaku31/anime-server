//! 统一管理媒体元数据缓存, 活动播放缓存和磁盘淘汰.

use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File, future::Future, path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Arc},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use tokio::{process::Child, sync::{watch, Mutex, Notify, OwnedSemaphorePermit}, task::JoinSet, time::interval};
use tracing::{info, warn};

use sqlx::SqlitePool;

use crate::{cache::metadata::MetadataEntry, config::PlaybackConfig, db, utils};

mod disk;
mod metadata;

use disk::{disk_usage, persist_used};
use crate::video::{
    self, CachedMedia, EncodeSpec, RenderAssets,
    Init, Tail, cancelled, check_cancel, read_tail, Cancelled, Processes,
};
use crate::models::PlaybackVariantConfig;


static REVISION: AtomicU64 = AtomicU64::new(1);

// 进程实际退出时立即归还预算, 不把后续磁盘提交算成正在编码.
type EncoderBudget = Option<(OwnedSemaphorePermit, OwnedSemaphorePermit, OwnedSemaphorePermit)>;


/// 缓存文件夹状态, 对外仅提供克隆快照和只读 getter.
#[derive(Clone)]
pub(crate) struct FolderState {
    media: CachedMedia,
    // 恢复完成前的空 media 只是占位, 不能作为创建响应返回.
    loaded: bool,
    target: Option<f64>,
    // worker 覆盖整个任务生命周期, encoding 仅表示编码进程仍在运行.
    worker_active: bool,
    encoding: bool,
    // 开始最终清理后不能撤销停止, 新租约必须等待旧写入者退出.
    finalizing: bool,
    encoding_threads: usize,
    stop_after: Option<usize>,
    stop_deadline: Option<Instant>,
    cancel: watch::Sender<bool>,
    revision: u64,
    last_used: SystemTime,
    error: Option<String>,
}

impl FolderState {
    // 媒体完整性与任务生命周期独立, 完整缓存仍可能处于收尾阶段.
    pub(crate) fn complete(&self) -> bool { self.media.complete }
    pub(crate) fn generating(&self) -> bool { self.worker_active }
    pub(crate) fn stopping(&self) -> bool { self.stop_after.is_some() || self.finalizing }
    pub(crate) fn ready(&self) -> bool { !self.media.segments.is_empty() }
    pub(crate) fn buffered_until(&self) -> f64 { self.media.tail().seconds() }
    pub(crate) fn encoding_threads(&self) -> usize { self.encoding_threads }
    pub(crate) fn revision(&self) -> u64 { self.revision }
    pub(crate) fn error(&self) -> Option<&str> { self.error.as_deref() }
}

/// 媒体文件打开期间的缓存保护对象.
///
/// `SessionManager::media_path` 返回其 `Arc`, 调用方持有它直到文件打开完成,
/// 防止最后会话释放后 LRU 在文件打开之前删除缓存.
/// 外部不能直接创建, 查询或修改该对象, 媒体访问仍须经过有效会话.
/// 同一配置的全部会话共享此元素, 只有其生成任务能发布正式分片.
struct CacheFolder {
    key: String,
    directory: PathBuf,
    spec: EncodeSpec,
    state: Mutex<FolderState>,
    changed: Notify,
}

/// 文件打开期间持有缓存文件夹, 避免无活动会话时被 LRU 回收.
pub(crate) struct CachePin {
    _folder: Arc<CacheFolder>,
}

impl CacheFolder {
    async fn wait_loaded(&self) -> Result<()> {
        // 创建响应必须反映磁盘的真实完成状态, 等待期间不占用全局活动集合锁.
        loop {
            // 先注册通知再检查条件, 避免恢复完成通知落在检查与等待之间.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let state = self.state.lock().await;
            if state.loaded { return Ok(()); }
            ensure!(state.worker_active, "cache activation failed: {}",
                state.error.as_deref().unwrap_or("generation stopped before loading"));
            drop(state);
            notified.await;
        }
    }

    async fn status(&self) -> Result<FolderState> {
        let mut state = self.state.lock().await;
        state.target.context("playback cache has no active session")?;
        state.last_used = SystemTime::now();
        Ok(state.clone())
    }

    fn stop(state: &mut FolderState) {
        if state.encoding {
            // 只允许再提交一个分片. 已在私有目录生成的更远内容也不能越过此边界发布.
            state.stop_after.get_or_insert(state.media.segments.len() + 1);
            state.stop_deadline.get_or_insert(Instant::now() + Duration::from_secs(30));
        } else {
            state.cancel.send_replace(true);
        }
    }
}

struct CacheInner {
    root: PathBuf,
    config: PlaybackConfig,
    database: SqlitePool,
    processes: Processes,
    metadata: Mutex<HashMap<String, MetadataEntry>>,
    // 同时访问集合和元素时先锁 entries 再锁 state, 创建与回收必须保持相同顺序.
    entries: Mutex<HashMap<String, Arc<CacheFolder>>>,
    workers: Mutex<JoinSet<()>>,
    maintenance: Mutex<()>,
    stop: watch::Sender<bool>,
    _directory_lock: File,
}

/// 缓存统一入口, 独占目录锁并管理两个按需加载的 Map.
///
/// new 校验缓存根目录并初始化临时资源目录和容量维护.
/// media_info 查询或重试成功元数据缓存, acquire 激活指定播放配置的共享文件夹.
/// status 返回字段私有的 FolderState 快照, duration 独立读取不可变执行参数.
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
        for name in ["playback", "subtitles", "fonts"] {
            let directory = root.join(name);
            tokio::fs::create_dir_all(&directory).await?;
            ensure!(tokio::fs::symlink_metadata(&directory).await?.is_dir(), "unsafe cache directory: {name}");
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

    pub(crate) fn stopping(&self) -> bool { *self.inner.stop.borrow() }

    // 查找或创建共享缓存
    pub(crate) async fn acquire(&self, config: &PlaybackVariantConfig) -> Result<()> {
        let key = config.key().to_owned();
        loop {
            let entries = self.inner.entries.lock().await;
            ensure!(!self.stopping(), "server is shutting down");
            if let Some(output) = entries.get(&key).cloned() {
                let notified = output.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let mut state = output.state.lock().await;
                ensure!(output.spec.config == *config, "playback configuration hash collision");
                if state.finalizing || (state.worker_active && *state.cancel.borrow()) {
                    // 已开始回收的任务不可复活, 等待旧写入者退出后再从磁盘加载.
                    drop(state);
                    drop(entries);
                    notified.await;
                    continue;
                }
                state.last_used = SystemTime::now();
                state.target = Some(0.0);
                state.stop_after = None;
                state.stop_deadline = None;
                if !state.media.complete && !state.worker_active {
                    state.worker_active = true;
                    state.error = None;
                    state.cancel = watch::channel(false).0;
                    state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
                    self.spawn(&output).await;
                }
                drop(state);
                return Ok(());
            }
            ensure!(entries.len() < 32, "too many active playback configurations");
            drop(entries);
            let info = self.media_info(config.src_file_pre16mib_md5(), false).await?;
            let source = db::get_a_video_path_by_id(&self.inner.database, config.src_file_pre16mib_md5()).await?
                .context("video file not found")?;
            let spec = EncodeSpec::from_config(config.clone(), source, info, 0.0)?;
            let mut entries = self.inner.entries.lock().await;
            ensure!(!self.stopping(), "server is shutting down");
            if entries.contains_key(&key) { continue; }
            ensure!(entries.len() < 32, "too many active playback configurations");
            let output = Arc::new(CacheFolder {
                key: key.to_string(), directory: self.inner.root.join("playback").join(&key), spec: spec.clone(),
                state: Mutex::new(FolderState {
                    media: CachedMedia::default(), loaded: false, target: Some(spec.target),
                    worker_active: true, encoding: false, finalizing: false, encoding_threads: 0,
                    stop_after: None, stop_deadline: None, cancel: watch::channel(false).0,
                    revision: REVISION.fetch_add(1, Ordering::Relaxed), last_used: SystemTime::now(), error: None,
                }),
                changed: Notify::new(),
            });
            // 先保留唯一活动元素, 磁盘校验在该元素的任务内进行, 不阻塞其他会话的 DELETE.
            match entries.entry(key.to_string()) {
                Entry::Vacant(entry) => { entry.insert(output.clone()); },
                Entry::Occupied(_) => bail!("playback activation raced while holding its lock"),
            }
            self.spawn(&output).await;
            return Ok(());
        }
    }

    async fn spawn(&self, output: &Arc<CacheFolder>) {
        let manager = self.clone();
        let output = output.clone();
        let processes = self.inner.processes.clone();
        self.inner.workers.lock().await.spawn(async move {
            manager.generate(&processes, &output).await;
            drop(output);
            if let Err(error) = manager.maintain().await { warn!("cache maintenance failed: {error:#}"); }
        });
    }

    async fn get(&self, key: &str) -> Result<Arc<CacheFolder>> {
        self.inner.entries.lock().await.get(key).cloned().context("playback cache is no longer active")
    }

    pub(crate) async fn wait_loaded(&self, key: &str) -> Result<()> {
        self.get(key).await?.wait_loaded().await
    }

    pub(crate) async fn status(&self, key: &str) -> Result<FolderState> {
        self.get(key).await?.status().await
    }

    /// 时长来自不可变编码参数, 不在状态快照中重复保存.
    pub(crate) async fn duration(&self, key: &str) -> Result<f64> {
        Ok(self.get(key).await?.spec.duration)
    }

    /// 更新等待目标但不重建缓存, 运行中的编码进程也不会因此热调线程.
    pub(crate) async fn target(&self, key: &str, target: f64) -> Result<()> {
        let output = self.get(key).await?;
        ensure!(target.is_finite() && target >= 0.0 && target < output.spec.duration, "position must be within the video duration");
        let mut state = output.state.lock().await;
        state.target = Some(target);
        state.last_used = SystemTime::now();
        Ok(())
    }

    /// 最后一个会话离开时解除目标并停止缓存生成.
    pub(crate) async fn release(&self, key: &str) -> Result<()> {
        let entries = self.inner.entries.lock().await;
        let Some(output) = entries.get(key).cloned() else { return Ok(()); };
        let mut state = output.state.lock().await;
        if state.target.take().is_none() { return Ok(()); }
        state.last_used = SystemTime::now();
        if state.worker_active {
            CacheFolder::stop(&mut state);
        } else {
            let persisted = persist_used(&output.directory, state.last_used).await;
            output.changed.notify_waiters();
            // 即使时间戳落盘失败, 后续维护也会清除无会话的文件夹记录.
            persisted?;
        }
        // 不等待 FFmpeg 完成当前分片. 回收和 LRU 由生成任务或定期维护完成.
        Ok(())
    }

    /// 只开放正式清单中的已发布资源, 私有工作目录和未验证尾部不能被读取.
    pub(crate) async fn media_path(&self, key: &str, name: &str) -> Result<(PathBuf, CachePin)> {
        ensure!(video::media_filename(name), "playback asset not found");
        let output = self.get(key).await?;
        let mut state = output.state.lock().await;
        ensure!(state.target.is_some(), "playback cache has no active session");
        ensure!(!state.media.segments.is_empty(), "playback output is not ready");
        if let Some(index) = name.strip_prefix("segment_").and_then(|name| name.strip_suffix(".m4s")) {
            ensure!(index.parse::<usize>()? < state.media.segments.len(), "playback segment is not published");
        }
        state.last_used = SystemTime::now();
        let path = output.directory.join(name);
        let file = tokio::fs::symlink_metadata(&path).await.context("cached media file is missing")?;
        ensure!(file.is_file() && file.len() > 0, "unsafe or empty cached media file");
        drop(state);
        Ok((path, CachePin { _folder: output }))
    }

    // 成功, 失败和取消统一收尾, 有会话时保留错误快照.
    async fn generate(&self, processes: &Processes, output: &Arc<CacheFolder>) {
        let mut cancel = output.state.lock().await.cancel.subscribe();
        let result = self.run(processes, output, &mut cancel).await;
        let _entries = self.inner.entries.lock().await;
        let mut state = output.state.lock().await;
        state.finalizing = true;
        if let Err(error) = result {
            if !error.is::<Cancelled>() || state.target.is_some() {
                state.error = Some(format!("{error:#}"));
                warn!(variant_key = output.key, "cache generation failed: {error:#}");
            }
        }
        state.worker_active = false;
        state.encoding = false;
        state.encoding_threads = 0;
        state.stop_after = None;
        state.stop_deadline = None;
        if let Err(error) = persist_used(&output.directory, state.last_used).await {
            warn!(variant_key = output.key, "cache timestamp update failed: {error:#}");
            state.error = Some(format!("cache timestamp update failed: {error:#}"));
        }
        state.finalizing = false;
        output.changed.notify_waiters();
    }

    // 先恢复有效磁盘前缀, 再决定复用完整缓存, 确认 EOF 或从尾部继续生成.
    async fn run(&self, processes: &Processes, output: &Arc<CacheFolder>, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        match tokio::fs::create_dir(&output.directory).await {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                ensure!(tokio::fs::symlink_metadata(&output.directory).await?.is_dir(), "unsafe cache directory"),
            Err(error) => return Err(error.into()),
        }
        let media = video::recover(&output.directory, processes, cancel).await?;
        let resume = media.tail();
        let complete = media.complete;
        {
            let mut state = output.state.lock().await;
            state.media = media;
            state.loaded = true;
        }
        output.changed.notify_waiters();
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
        // 排队时只取基础预算, 等到真正启动前才按最新客户端目标申请额外线程.
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
            ensure!(state.target.is_some(), "cache generation has no active session");
            let desired = if state.target.is_some_and(|target| target >= state.media.tail().seconds()) {
                self.inner.config.max_variant_threads
            } else { base };
            processes.extend_encoding_threads(&mut threads, desired)?;
            let child = video::command(&output.spec, directory, assets, resume, threads.num_permits())?
                .spawn().context("failed to start FFmpeg")?;
            state.encoding = true;
            state.encoding_threads = threads.num_permits();
            child
        };
        info!(variant_key = output.key, pid = child.id(), threads = threads.num_permits(),
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
        if state.stop_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            state.cancel.send_replace(true);
            warn!(variant_key = output.key, "forcing cache stop after the 30 second segment deadline");
        }
        check_cancel(cancel)?;
        if let Some(exit) = child.try_wait()? {
            drop(budget.take());
            state.encoding = false;
            state.encoding_threads = 0;
            ensure!(exit.success(), "FFmpeg exited with {exit}");
        }
        ensure!(published > 0 || started.elapsed().as_secs() < self.inner.config.startup_timeout_seconds,
            "FFmpeg startup timed out");
        Ok(())
    }

    // 长耗时校验与编码监控同时推进, 保证等待校验时仍能响应停止和启动超时.
    async fn checked_step<T>(&self, output: &CacheFolder, child: &mut Child, budget: &mut EncoderBudget, started: Instant,
        published: usize, cancel: &watch::Receiver<bool>, step: impl Future<Output = Result<T>>) -> Result<T> {
        let stop = output.state.lock().await.cancel.clone();
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
                                warn!(variant_key = output.key, "interrupted media step failed: {cleanup:#}");
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
                let mut state = output.state.lock().await;
                if state.stop_after.is_some_and(|count| state.media.segments.len() >= count) {
                    state.finalizing = true;
                    return Ok(());
                }
            } else {
                ensure!(!finished, "FFmpeg completed without an HLS playlist");
            }
            tick.tick().await;
        }
    }

    // 新分片必须通过 init 兼容, 时间轴和真实解码校验, 不能覆盖任何已发布字节.
    async fn publish(&self, processes: &Processes, output: &CacheFolder, directory: &Path, name: &str, resume: Tail,
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
        let data = video::regular_bytes(&directory.join(name), 256 * 1024 * 1024).await?;
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
        tokio::fs::remove_file(directory.join(name)).await?;
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
        state.finalizing = true;
        info!(variant_key = output.key, "playback cache complete");
        Ok(())
    }

    /// 按磁盘访问时间淘汰非活动缓存, 正在打开媒体文件的旧引用仍受保护.
    pub(crate) async fn maintain(&self) -> Result<()> {
        if let Ok(mut workers) = self.inner.workers.try_lock() {
            while let Some(result) = workers.try_join_next() {
                if let Err(error) = result { warn!("cache worker failed: {error}"); }
            }
        }
        let _maintenance = self.inner.maintenance.lock().await;
        {
            let mut entries = self.inner.entries.lock().await;
            let mut removable = Vec::new();
            for (key, folder) in entries.iter() {
                if Arc::strong_count(folder) != 1 { continue; }
                let state = folder.state.lock().await;
                if state.target.is_none() && !state.worker_active && !state.finalizing {
                    removable.push(key.clone());
                }
            }
            for key in removable { entries.remove(&key); }
        }
        let root = self.inner.root.clone();
        let (mut total, mut candidates) = tokio::task::spawn_blocking(move || disk_usage(&root)).await??;
        candidates.sort_by_key(|candidate| candidate.0);
        for (_, key, size) in candidates {
            if total <= self.inner.config.cache_max_bytes { break; }
            let entries = self.inner.entries.lock().await;
            if entries.contains_key(&key) {
                continue;
            }
            let path = self.inner.root.join("playback").join(&key);
            let trash = self.inner.root.join("playback").join(format!(".evict-{key}-{}", utils::random_id()?));
            // 先原子移走不活跃目录, 避免长时间删除文件阻塞其他缓存的状态和释放请求.
            tokio::fs::rename(&path, &trash).await?;
            drop(entries);
            tokio::fs::remove_dir_all(trash).await?;
            total = total.saturating_sub(size);
            info!(variant_key = key, "evicted inactive playback cache");
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
                state.target = None;
                if state.worker_active { CacheFolder::stop(&mut state); }
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
}

// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

#[cfg(test)]
mod tests {
    // use super::*;

    // // 构造已恢复的缓存文件夹, 不启动媒体进程或访问真实视频.
    // fn folder() -> CacheFolder {
    //         let (info, request) = video::fixture();
    //     let config = PlaybackVariantConfig::new(&"a".repeat(32), &info, &request).unwrap();
    //     CacheFolder {
    //         key: config.key().to_owned(),
    //         directory: PathBuf::new(),
    //         spec: EncodeSpec::from_config(config, "video.mkv".into(), info, 0.0).unwrap(),
    //         state: Mutex::new(FolderState {
    //             media: CachedMedia {
    //                 complete: true, init: None,
    //                     segments: vec![video::Fragment {
    //                     start: Tail::default(), end: Tail { video: 360000, audio: 192000 }, sequence: 1,
    //                 }],
    //             },
    //             loaded: true, target: Some(0.0), worker_active: true, encoding: false,
    //             finalizing: true, encoding_threads: 2, stop_after: None, stop_deadline: None,
    //             cancel: watch::channel(false).0, revision: 42,
    //             last_used: SystemTime::UNIX_EPOCH, error: Some("fixture error".to_owned()),
    //         }),
    //         changed: Notify::new(),
    //     }
    // }

    // #[tokio::test]
    // async fn folder_snapshot_preserves_status_and_is_independent_of_later_updates() {
    //     let folder = folder();
    //     let snapshot = folder.status().await.unwrap();
    //     assert!(snapshot.complete());
    //     assert!(snapshot.generating());
    //     assert!(snapshot.stopping());
    //     assert!(snapshot.ready());
    //     assert_eq!(snapshot.buffered_until(), 4.0);
    //     assert_eq!(snapshot.encoding_threads(), 2);
    //     assert_eq!(snapshot.revision(), 42);
    //     assert_eq!(snapshot.error(), Some("fixture error"));
    //     assert!(snapshot.last_used > SystemTime::UNIX_EPOCH);

    //     // 后续缓存更新不改变已返回快照中的媒体前缀和错误.
    //     {
    //         let mut state = folder.state.lock().await;
    //         state.media = CachedMedia::default();
    //         state.worker_active = false;
    //         state.finalizing = false;
    //         state.encoding_threads = 0;
    //         state.error = None;
    //     }
    //     let updated = folder.status().await.unwrap();
    //     assert!(!updated.complete() && !updated.generating() && !updated.stopping() && !updated.ready());
    //     assert_eq!(updated.buffered_until(), 0.0);
    //     assert_eq!(updated.encoding_threads(), 0);
    //     assert_eq!(updated.error(), None);
    //     assert!(snapshot.complete() && snapshot.ready());
    //     assert_eq!(snapshot.buffered_until(), 4.0);
    //     assert_eq!(snapshot.error(), Some("fixture error"));

    //     folder.state.lock().await.stop_after = Some(2);
    //     assert!(folder.status().await.unwrap().stopping());
    //     folder.state.lock().await.target = None;
    //     assert!(folder.status().await.is_err());
    // }

    // #[tokio::test]
    // async fn manager_reads_duration_separately_from_folder_snapshot() {
    //     let root = std::env::temp_dir().join(format!("anime-state-snapshot-{}", utils::random_id().unwrap()));
    //     tokio::fs::create_dir(&root).await.unwrap();
    //     let database = sqlx::sqlite::SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
    //     let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), database, Processes::new(2, 16)).await.unwrap();
    //     let folder = Arc::new(folder());
    //     let key = folder.key.clone();
    //     manager.inner.entries.lock().await.insert(key.clone(), folder);
    //     assert_eq!(manager.duration(&key).await.unwrap(), 20.0);
    //     assert_eq!(manager.status(&key).await.unwrap().revision(), 42);
    //     manager.inner.entries.lock().await.remove(&key);
    //     assert!(manager.duration(&key).await.is_err());
    //     assert!(manager.status(&key).await.is_err());
    //     drop(manager);
    //     tokio::fs::remove_dir_all(root).await.unwrap();
    // }

}


// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::video::models::PlaybackVariantConfig;

//     #[test]
//     fn configuration_serialization_is_canonical_and_excludes_runtime_fields() {
//         let config = PlaybackVariantConfig {
//             src_file_pre16mib_md5: "35af6d912b6dffadc112df2ca94bbe2f".to_string(),
//             video_stream: 0, audio_stream: Some(1), subtitle_stream: Some(3),
//             encoder_profile: "h264-high-yuv420p-aac-fmp4-v1".to_string(),
//         };
//         assert_eq!(config.key().unwrap(), "bc892e2d4aa401399639fe22ee897b43");
//         let mut updated = config.clone();
//         updated.encoder_profile = ENCODER_PROFILE.to_string();
//         assert_ne!(updated.key().unwrap(), config.key().unwrap());
//         updated.audio_stream = None;
//         updated.subtitle_stream = None;
//         assert!(String::from_utf8(serde_json::to_vec(&updated).unwrap()).unwrap()
//             .contains("\"audio_stream\":null,\"subtitle_stream\":null"));
//         for key in ["../main.db", "ABCDEF0123456789ABCDEF0123456789", "a", ""] {
//             assert!(!is_key(key));
//         }
//     }

//     #[tokio::test]
//     async fn startup_does_not_activate_disk_entries_and_lru_includes_partial_caches() {
//         // 不完整和完整目录都参与 LRU, 未知文件和目录保持不动.
//         let root = std::env::temp_dir().join(format!("anime-active-lru-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let config = PlaybackConfig { cache_max_bytes: 80, ..PlaybackConfig::default() };
//         let manager = CacheManager::new(root.clone(), config, Processes::new(2, 16)).await.unwrap();
//         for (name, age) in [('a', 1), ('b', 2)] {
//             let directory = root.join("playback").join(name.to_string().repeat(32));
//             tokio::fs::create_dir(&directory).await.unwrap();
//             tokio::fs::write(directory.join("segment_000000.m4s"), vec![0; 60]).await.unwrap();
//             if name == 'b' { tokio::fs::write(directory.join("complete"), []).await.unwrap(); }
//             persist_used(&directory, SystemTime::UNIX_EPOCH + Duration::from_secs(age)).await.unwrap();
//         }
//         assert!(manager.inner.entries.lock().await.is_empty());
//         manager.maintain().await.unwrap();
//         assert!(!root.join("playback").join("a".repeat(32)).exists());
//         assert!(root.join("playback").join("b".repeat(32)).exists());
//         assert!(CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(2, 16)).await.is_err());
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }

//     #[tokio::test]
//     async fn last_lease_retires_the_entry_but_an_open_pin_still_blocks_eviction() {
//         // 仅模拟已经发布的文件, 区分活动租约保护和退出集合后的文件打开保护.
//         use tokio::io::AsyncReadExt;
//         let root = std::env::temp_dir().join(format!("anime-media-pin-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let config = PlaybackConfig { cache_max_bytes: 1, ..PlaybackConfig::default() };
//         let manager = CacheManager::new(root.clone(), config, Processes::new(2, 16)).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let key = spec.config.key().unwrap();
//         let directory = root.join("playback").join(&key);
//         tokio::fs::create_dir(&directory).await.unwrap();
//         tokio::fs::write(directory.join("init.mp4"), b"file-open fixture").await.unwrap();
//         let output = Arc::new(ActiveCache {
//             key: key.clone(), directory: directory.clone(), spec: spec.clone(), changed: Notify::new(),
//             state: Mutex::new(ActiveState {
//                 media: CachedMedia { complete: true, init: None, segments: vec![fragment::Fragment {
//                     start: Tail::default(), end: Tail { video: 360000, audio: 192000 }, sequence: 1,
//                 }] },
//                 loaded: true, clients: HashMap::from([("first".to_string(), 0.0)]),
//                 worker_active: false, encoding: false, finalizing: false, encoding_threads: 0,
//                 stop_after: None, stop_deadline: None, cancel: watch::channel(false).0,
//                 revision: 1, last_used: SystemTime::now(), error: None,
//             }),
//         });
//         manager.inner.entries.lock().await.insert(key.clone(), output);
//         let shared = manager.acquire(spec, "second").await.unwrap();
//         assert_eq!(shared.state.lock().await.clients.len(), 2);
//         manager.maintain().await.unwrap();
//         assert!(directory.exists());
//         let (path, pin) = manager.media_path(&key, "second", "init.mp4").await.unwrap();
//         manager.release(&key, "first").await.unwrap();
//         manager.release(&key, "first").await.unwrap();
//         assert_eq!(shared.state.lock().await.clients.len(), 1);
//         drop(shared);
//         manager.release(&key, "second").await.unwrap();
//         assert!(manager.get(&key).await.is_err());
//         assert!(pin.status("second").await.is_err());
//         manager.maintain().await.unwrap();
//         assert!(directory.exists());
//         let mut file = tokio::fs::File::open(path).await.unwrap();
//         drop(pin);
//         manager.maintain().await.unwrap();
//         assert!(!directory.exists());
//         let mut data = Vec::new();
//         file.read_to_end(&mut data).await.unwrap();
//         assert_eq!(data, b"file-open fixture");
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }

//     #[tokio::test]
//     async fn stop_is_revocable_but_a_finalizing_writer_must_exit_before_reactivation() {
//         // 占满进程名额, 让测试只控制租约和停止边界, 不依赖真实编码速度.
//         let root = std::env::temp_dir().join(format!("anime-reactivate-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let processes = Processes::new(2, 16);
//         let held = processes.slots.acquire_many(2).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec.clone(), "first").await.unwrap();
//         output.wait_loaded().await.unwrap();
//         output.state.lock().await.encoding = true;
//         manager.release(&output.key, "first").await.unwrap();
//         let deadline = {
//             let mut state = output.state.lock().await;
//             assert_eq!(state.stop_after, Some(1));
//             let deadline = state.stop_deadline.unwrap();
//             ActiveCache::stop(&mut state);
//             assert_eq!(state.stop_deadline, Some(deadline));
//             deadline
//         };
//         assert!(deadline > Instant::now());
//         let shared = manager.acquire(spec.clone(), "second").await.unwrap();
//         assert!(Arc::ptr_eq(&output, &shared));
//         {
//             let mut state = output.state.lock().await;
//             assert_eq!(state.clients.len(), 1);
//             assert!(state.stop_after.is_none() && state.stop_deadline.is_none());
//             assert!(!*state.cancel.borrow());
//             state.encoding = false;
//             state.finalizing = true;
//         }
//         let waiting = manager.acquire(spec, "third");
//         tokio::pin!(waiting);
//         assert!(tokio::time::timeout(Duration::from_millis(30), &mut waiting).await.is_err());
//         manager.release(&output.key, "second").await.unwrap();
//         let next = tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap().unwrap();
//         assert!(!Arc::ptr_eq(&output, &next));
//         assert_eq!(manager.inner.entries.lock().await.len(), 1);
//         manager.release(&next.key, "third").await.unwrap();
//         manager.shutdown().await.unwrap();
//         drop(held);
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }

//     #[tokio::test]
//     async fn activation_io_failure_does_not_leave_an_unleased_active_entry() {
//         // 用普通文件阻断目录创建, 同时触发恢复失败和最后释放时的 mtime 写入失败.
//         let root = std::env::temp_dir().join(format!("anime-activation-failure-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(2, 16)).await.unwrap();
//         let playback = root.join("playback");
//         tokio::fs::remove_dir(&playback).await.unwrap();
//         tokio::fs::write(&playback, b"not a directory").await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec, "failed").await.unwrap();
//         assert!(output.wait_loaded().await.is_err());
//         assert!(manager.release(&output.key, "failed").await.is_err());
//         assert!(manager.get(&output.key).await.is_err());
//         tokio::fs::remove_file(&playback).await.unwrap();
//         tokio::fs::create_dir(&playback).await.unwrap();
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }

//     #[tokio::test]
//     async fn publication_monitor_never_waits_for_the_future_it_is_polling() {
//         // 模拟提交持锁等待磁盘, 防止监控分支与同一 select 中的提交 future 自锁.
//         let root = std::env::temp_dir().join(format!("anime-publication-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let processes = Processes::new(2, 16);
//         let held = processes.slots.acquire_many(2).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec, "test").await.unwrap();
//         let cancel = output.state.lock().await.cancel.subscribe();
//         let mut child = tokio::process::Command::new("sleep").arg("5").kill_on_drop(true).spawn().unwrap();
//         let step = async {
//             let _state = output.state.lock().await;
//             tokio::time::sleep(Duration::from_millis(150)).await;
//             Ok(42)
//         };
//         let result = tokio::time::timeout(Duration::from_secs(1),
//             manager.checked_step(&output, &mut child, &mut None, Instant::now(), 0, &cancel, step)).await;
//         child.kill().await.unwrap();
//         child.wait().await.unwrap();
//         manager.release(&output.key, "test").await.unwrap();
//         manager.shutdown().await.unwrap();
//         drop(held);
//         tokio::fs::remove_dir_all(root).await.unwrap();
//         assert_eq!(result.unwrap().unwrap(), 42);
//     }
// }
