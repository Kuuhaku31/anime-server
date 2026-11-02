
// backend/src/cache/handle.rs
//! 维护单个缓存实例的六状态生命周期和连续分片发布.

use std::{future::Future, path::{Path, PathBuf}, sync::Arc, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use tokio::{sync::{watch, Mutex}, time::{interval, sleep}};
use tracing::{info, warn};

use crate::{
    config::AppConfigManager, models::{CacheSpec, MediaMetadata},
    process::{self, check_cancel, CachedMedia, Cancelled, EncodingWorker, Init, ProcessManager, RenderAssets, Tail, WorkerUsage},
    utils,
};

use super::{CacheError, CacheSnapshot, CacheStatus};

#[cfg(test)]
#[path = "../../tests/cache/handle.rs"]
mod tests;

/// 保存稳定身份, 唯一目录, 阶段锁和资源计数, 媒体进度以磁盘清单为准.
pub(super) struct CacheInstanceHandle {
    /// 与实例描述和目录哈希一致的稳定身份.
    pub(super) spec: CacheSpec,
    /// 稳定身份的哈希, 同时作为目录名称.
    pub(super) handle_id: String,
    /// 本句柄唯一对应的实际缓存目录.
    pub(super) path: PathBuf,
    /// None 仅表示执行 End 时已封闭的句柄, 不作为可查询状态.
    phase: Mutex<Option<Phase>>,
    /// 长时间转码 worker 的资源计数, 不持有 worker 或其他管理器.
    resources: Arc<WorkerUsage>,
}

/// 六个内部阶段及其必要的启动和取消数据.
enum Phase {
    /// 恢复任务只能由第一个调用者启动.
    Loading {
        /// 是否已经启动恢复任务.
        started: bool,
        /// 恢复任务的取消信号, 关闭接收端表示任务已退出.
        cancel: watch::Sender<bool>,
    },
    /// 已完成的实例不再启动生成.
    Completed,
    /// 已恢复的实例等待下一次生成请求.
    Ready {
        /// 保留恢复阶段的取消通道, 让 stop 能回收尚未获得启动权的请求.
        cancel: watch::Sender<bool>,
    },
    /// 保存恢复或生成失败的诊断.
    Failed {
        /// 最近一次失败的详细原因.
        error: String,
    },
    /// 正在等待编码和校验任务回收.
    Stopping {
        /// 与运行任务共享的取消信号.
        cancel: watch::Sender<bool>,
    },
    /// 正在排队或执行生成任务.
    Running {
        /// 当前生成任务的取消信号.
        cancel: watch::Sender<bool>,
    },
}

impl CacheInstanceHandle {
    /// 为一个已建立目录的稳定身份创建未启动的 Loading 句柄.
    pub(super) fn new(spec: CacheSpec, handle_id: String, path: PathBuf) -> Self {
        Self { spec, handle_id, path, phase: Mutex::new(Some(Phase::loading())),
            resources: Arc::new(WorkerUsage::default()) }
    }

    /// 查询阶段和已发布清单, 不读取尚未恢复的损坏尾部.
    pub(super) async fn snapshot(&self) -> Result<CacheSnapshot> {
        let lock = self.phase.lock().await;
        let phase = lock.as_ref().ok_or(CacheError::NotFound)?;
        let buffered_until = if matches!(phase, Phase::Loading { .. }) { 0.0 }
        else {
            process::read_playlist(&self.path).await?.map_or(0.0, |playlist| playlist.duration)
        };
        let error = match phase { Phase::Failed { error } => Some(error.clone()), _ => None };
        Ok(CacheSnapshot {
            handle_id: self.handle_id.clone(), spec: self.spec.clone(),
            status: CacheStatus { phase: phase.kind(), buffered_until, error },
            resources: self.resources.snapshot(),
            source_path: None,
        })
    }

    /// 原子处理首次加载或 Failed 重试, 恢复工作不持有阶段锁.
    pub(super) async fn load(self: &Arc<Self>, processes: &ProcessManager) -> Result<()> {
        let (sender, mut cancel) = {
            let mut phase = self.phase.lock().await;
            if matches!(phase.as_ref(), Some(Phase::Failed { .. })) {
                transition(&mut phase, Some(Phase::loading()))?;
            }
            match phase.as_mut().ok_or(CacheError::NotFound)? {
                Phase::Loading { started, cancel } if !*started => {
                    *started = true;
                    (cancel.clone(), cancel.subscribe())
                },
                _ => return Ok(()),
            }
        };
        let handle = self.clone();
        let processes = processes.clone();
        tokio::spawn(async move {
            let result = processes.recover(&handle.path, &mut cancel).await.and_then(|media| {
                check_cancel(&cancel)?;
                Ok(media)
            });
            let next = match result {
                Ok(media) if media.complete => Phase::Completed,
                Ok(_) => Phase::Ready { cancel: sender },
                Err(error) => {
                    warn!(handle_id = handle.handle_id, "cache recovery failed: {error:#}");
                    Phase::Failed { error: format!("{error:#}") }
                },
            };
            if let Err(error) = transition(&mut *handle.phase.lock().await, Some(next)) {
                warn!(handle_id = handle.handle_id, "cache recovery transition failed: {error:#}");
            }
        });
        Ok(())
    }

    /// 立即返回并由内部任务等待 Loading, 只有 Ready 的赢家能启动编码.
    pub(super) async fn run(
        self: &Arc<Self>, source_path: PathBuf, metadata: MediaMetadata,
        config: &AppConfigManager, processes: &ProcessManager,
    ) -> Result<()> {
        let waiting = {
            let mut phase = self.phase.lock().await;
            match phase.as_ref().ok_or(CacheError::NotFound)? {
                Phase::Running { .. } | Phase::Completed => return Ok(()),
                Phase::Stopping { .. } => return Err(CacheError::Stopping.into()),
                Phase::Failed { error } => bail!("cache instance failed: {error}"),
                Phase::Ready { .. } => {
                    return self.spawn_generation(&mut phase, source_path, metadata, config, processes);
                },
                Phase::Loading { cancel, .. } => cancel.subscribe(),
            }
        };
        let handle = self.clone();
        let config = config.clone();
        let processes = processes.clone();
        tokio::spawn(async move {
            if let Err(error) = handle.run_when_ready(waiting, source_path, metadata, config, processes).await {
                warn!(handle_id = handle.handle_id, "cache run request failed: {error:#}");
            }
        });
        Ok(())
    }

    /// 幂等停止当前任务并等接收端退出, 返回是否经历了 Stopping.
    pub(super) async fn stop(&self) -> Result<bool> {
        let mut stopped = false;
        loop {
            let cancel = {
                let mut phase = self.phase.lock().await;
                match phase.as_ref() {
                    Some(Phase::Running { cancel }) => {
                        let cancel = cancel.clone();
                        transition(&mut phase, Some(Phase::Stopping { cancel: cancel.clone() }))?;
                        stopped = true;
                        cancel
                    },
                    Some(Phase::Stopping { cancel }) => {
                        stopped = true;
                        cancel.clone()
                    },
                    Some(Phase::Loading { started: true, cancel }) => cancel.clone(),
                    Some(Phase::Ready { cancel }) => cancel.clone(),
                    Some(Phase::Failed { error }) if stopped => bail!("cache stop failed: {error}"),
                    _ => return Ok(stopped),
                }
            };
            cancel.send_replace(true);
            cancel.closed().await;
            let phase = self.phase.lock().await;
            match phase.as_ref() {
                Some(Phase::Stopping { .. }) => return Ok(true),
                Some(Phase::Ready { .. }) => return Ok(stopped),
                Some(Phase::Loading { .. }) => bail!("cache recovery task exited without a stable phase"),
                _ => {},
            }
        }
    }

    /// 用户停止只保留已发布前缀, 等全部写入者退出后恢复为可续接阶段.
    pub(super) async fn pause(&self) -> Result<()> {
        {
            let phase = self.phase.lock().await;
            if !matches!(phase.as_ref(), Some(Phase::Running { .. } | Phase::Stopping { .. })) {
                return Ok(());
            }
        }
        self.stop().await?;
        let mut phase = self.phase.lock().await;
        if matches!(phase.as_ref(), Some(Phase::Stopping { .. })) {
            let complete = process::read_playlist(&self.path).await?.is_some_and(|playlist| playlist.complete);
            let next = if complete { Phase::Completed }
                else { Phase::Ready { cancel: watch::channel(false).0 } };
            transition(&mut phase, Some(next))?;
        }
        Ok(())
    }

    /// 封闭句柄以阻止排队任务重新启动, 删除目录后允许管理器移除 Map 项.
    pub(super) async fn end(&self) -> Result<()> {
        loop {
            self.stop().await?;
            let mut phase = self.phase.lock().await;
            // 等待加载期间已经排队的 run 可能刚刚赢得启动权, 必须再次停止它.
            if matches!(phase.as_ref(), Some(Phase::Running { .. })) { continue; }
            if phase.is_some() { transition(&mut phase, None)?; }
            break;
        }
        match tokio::fs::remove_dir_all(&self.path).await {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error).context("cannot delete cache instance"),
        }
        process::sync_directory(self.path.parent().context("cache directory has no parent")?).await
    }

    /// 只开放清单中的已提交资源, 不暴露 descriptor 或私有工作文件.
    pub(super) async fn media_path(&self, asset_name: &str) -> Result<PathBuf> {
        ensure!(process::media_filename(asset_name), "playback asset not found");
        {
            let phase = self.phase.lock().await;
            let phase = phase.as_ref().ok_or(CacheError::NotFound)?;
            ensure!(!matches!(phase, Phase::Loading { .. }),
                "playback cache is not available");
        }
        let playlist = process::read_playlist(&self.path).await?.context("playback output is not ready")?;
        ensure!(!playlist.segments.is_empty(), "playback output is not ready");
        if asset_name.starts_with("segment_") {
            ensure!(playlist.segments.iter().any(|name| name == asset_name), "playback segment is not published");
        }
        let path = self.path.join(asset_name);
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        ensure!(metadata.is_file() && metadata.len() > 0, "unsafe or empty cached media asset");
        Ok(path)
    }

    /// 等待恢复结果并竞争唯一写入权, 请求数据只存在本次任务的栈上.
    async fn run_when_ready(
        self: &Arc<Self>, waiting: watch::Receiver<bool>, source_path: PathBuf, metadata: MediaMetadata,
        config: AppConfigManager, processes: ProcessManager,
    ) -> Result<()> {
        loop {
            {
                let mut phase = self.phase.lock().await;
                if *waiting.borrow() || waiting.has_changed().is_err() { return Ok(()); }
                match phase.as_ref() {
                    Some(Phase::Loading { .. }) => {},
                    Some(Phase::Ready { .. }) => {
                        return self.spawn_generation(&mut phase, source_path, metadata, &config, &processes);
                    },
                    Some(Phase::Failed { error }) => bail!("cache recovery failed: {error}"),
                    _ => return Ok(()),
                }
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    /// 在返回 run 前登记 Running 和取消接收端, 消除延迟启动与关闭的竞态.
    fn spawn_generation(
        self: &Arc<Self>, phase: &mut Option<Phase>, source_path: PathBuf, metadata: MediaMetadata,
        config: &AppConfigManager, processes: &ProcessManager,
    ) -> Result<()> {
        let (sender, mut cancel) = watch::channel(false);
        transition(phase, Some(Phase::Running { cancel: sender.clone() }))?;
        let handle = self.clone();
        let config = config.clone();
        let processes = processes.clone();
        tokio::spawn(async move {
            let result = handle.generate(&processes, &config, &source_path, &metadata, &sender, &mut cancel).await;
            let mut phase = handle.phase.lock().await;
            let next = match result {
                Ok(()) if matches!(phase.as_ref(), Some(Phase::Stopping { .. })) => return,
                Ok(()) => Phase::Completed,
                Err(error) if error.is::<Cancelled>() => {
                    if matches!(phase.as_ref(), Some(Phase::Stopping { .. })) { return; }
                    Phase::Ready { cancel: watch::channel(false).0 }
                },
                Err(error) => {
                    warn!(handle_id = handle.handle_id, "cache generation failed: {error:#}");
                    Phase::Failed { error: format!("{error:#}") }
                },
            };
            if let Err(error) = transition(&mut phase, Some(next)) {
                warn!(handle_id = handle.handle_id, "cache generation transition failed: {error:#}");
            }
        });
        Ok(())
    }

    /// 恢复已发布前缀并在私有目录执行续接, 不把媒体状态保存在句柄中.
    async fn generate(
        &self, processes: &ProcessManager, config: &AppConfigManager,
        source_path: &Path, metadata: &MediaMetadata, sender: &watch::Sender<bool>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let mut media = processes.recover(&self.path, cancel).await?;
        check_cancel(cancel)?;
        if media.complete { return Ok(()); }
        let resume = media.tail();
        if resume.video > 0 && !processes.has_video_after(&self.spec, source_path, metadata, resume, cancel).await? {
            return self.complete(&mut media).await;
        }
        let directory = self.path.join(format!(".work-{}", utils::random_id()?));
        tokio::fs::create_dir(&directory).await?;
        let result = self.render(processes, config, source_path, metadata, &directory, &mut media, sender, cancel).await;
        if let Err(cleanup) = tokio::fs::remove_dir_all(&directory).await {
            return Err(cleanup).context(match result {
                Ok(()) => "render succeeded but temporary directory cleanup failed".to_string(),
                Err(error) => format!("render failed: {error:#}; temporary directory cleanup failed"),
            });
        }
        result
    }

    /// 准备字幕后调用 ProcessManager 启动和回收编码进程.
    async fn render(
        &self, processes: &ProcessManager, config: &AppConfigManager, source_path: &Path,
        metadata: &MediaMetadata, directory: &Path, media: &mut CachedMedia,
        sender: &watch::Sender<bool>, cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let subtitle = metadata.subtitle_streams.iter()
            .find(|stream| Some(stream.stream_index) == self.spec.subtitle_stream_index);
        let assets = if subtitle.is_some_and(|stream| !stream.is_bitmap()) {
            let assets = RenderAssets::allocate(directory).await?;
            processes.prepare_assets(&assets, &self.spec, source_path, metadata, config.startup_timeout_seconds, cancel).await?;
            Some(assets)
        } else { None };
        let resume = media.tail();
        let mut command = processes.command(&self.spec, source_path, metadata, directory,
            assets.as_ref(), resume, config.ffmpeg_threads)?;
        let mut worker = processes.start_encoder(&mut command, config.ffmpeg_threads,
            config.startup_timeout_seconds, cancel, self.resources.clone()).await?;
        let result = self.monitor(processes, directory, media, resume, &mut worker, sender, cancel).await;
        worker.finish(result).await
    }

    /// 顺序消费私有 HLS 清单, 退出后也继续发布最后的已验证分片.
    async fn monitor(
        &self, processes: &ProcessManager, directory: &Path, media: &mut CachedMedia,
        resume: Tail, worker: &mut EncodingWorker, sender: &watch::Sender<bool>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let mut published = 0;
        let mut tick = interval(Duration::from_millis(100));
        loop {
            check_cancel(cancel)?;
            let finished = worker.check(published)?;
            if let Some(playlist) = process::read_playlist(directory).await? {
                if published < playlist.segments.len() {
                    let mut validation_cancel = cancel.clone();
                    self.checked_step(worker, published, sender, cancel, self.publish(
                        processes, directory, media, &playlist.segments[published], resume,
                        published == 0, &mut validation_cancel,
                    )).await?;
                    published += 1;
                }
                if finished && published == playlist.segments.len() {
                    ensure!(playlist.complete && published > 0, "FFmpeg completed without a playable final playlist");
                    return self.complete(media).await;
                }
            } else {
                ensure!(!finished, "FFmpeg completed without an HLS playlist");
            }
            tick.tick().await;
        }
    }

    /// 发布期间继续监控编码进程, 取消时等待校验进程自行回收而不丢弃 future.
    async fn checked_step<T>(
        &self, worker: &mut EncodingWorker, published: usize, sender: &watch::Sender<bool>,
        cancel: &watch::Receiver<bool>, step: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        tokio::pin!(step);
        let mut tick = interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                result = &mut step => return result,
                _ = tick.tick() => {
                    if let Err(error) = check_cancel(cancel).and_then(|_| worker.check(published)) {
                        sender.send_replace(true);
                        let interrupted = worker.interrupt();
                        if let Err(cleanup) = step.await {
                            if !cleanup.is::<Cancelled>() {
                                warn!(handle_id = self.handle_id, "interrupted media step failed: {cleanup:#}");
                            }
                        }
                        interrupted?;
                        return Err(error);
                    }
                },
            }
        }
    }

    /// 先验证 init, 时间轴和真实解码, 再原子发布且永不覆盖已发布分片.
    async fn publish(
        &self, processes: &ProcessManager, directory: &Path, media: &mut CachedMedia,
        source_name: &str, resume: Tail, first: bool, cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let init_bytes = process::regular_bytes(&directory.join("init.mp4"), 4 * 1024 * 1024).await?;
        let init = Init::parse(&init_bytes)?;
        ensure!(init.audio() == self.spec.audio_stream_index.is_some(), "unexpected generated audio track");
        if let Some(existing) = &media.init { existing.compatible(&init)?; }
        else {
            process::atomic_file(&self.path, "init.mp4", &init_bytes).await?;
            media.init = Some(init.clone());
        }
        let count = media.segments.len();
        ensure!(count < 1_000_000, "too many cached HLS segments");
        let data = process::regular_bytes(&directory.join(source_name), 256 * 1024 * 1024).await?;
        let (data, fragment) = process::normalize(&data, &init, resume, media.tail(), count as u32 + 1)?;
        process::atomic_file(directory, "ready.m4s", &data).await?;
        processes.validate_segment(directory, "../init.mp4", "ready.m4s", init.audio(), first, cancel).await?;
        check_cancel(cancel)?;
        let mut next = media.clone();
        next.segments.push(fragment);
        next.playlist()?;
        let path = self.path.join(process::segment_name(count));
        ensure!(!tokio::fs::try_exists(&path).await?, "refusing to overwrite a published cache segment");
        tokio::fs::rename(directory.join("ready.m4s"), path).await?;
        process::sync_directory(&self.path).await?;
        process::write_playlist(&self.path, &next).await?;
        *media = next;
        tokio::fs::remove_file(directory.join(source_name)).await?;
        Ok(())
    }

    /// 确认 EOF 后先提交 ENDLIST 再提交完成标记.
    async fn complete(&self, media: &mut CachedMedia) -> Result<()> {
        ensure!(!media.segments.is_empty(), "cannot complete an empty playback cache");
        media.complete = true;
        process::write_playlist(&self.path, media).await?;
        process::atomic_file(&self.path, "complete", b"").await?;
        info!(handle_id = self.handle_id, "playback cache completed");
        Ok(())
    }
}

impl Phase {
    /// 创建尚未启动恢复任务的初始阶段.
    fn loading() -> Self {
        Self::Loading { started: false, cancel: watch::channel(false).0 }
    }

    /// 将内部阶段映射为对外六状态之一.
    fn kind(&self) -> String {
        match self {
            Self::Loading { .. } => "loading".to_string(),
            Self::Completed => "completed".to_string(),
            Self::Ready { .. } => "ready".to_string(),
            Self::Failed { .. } => "failed".to_string(),
            Self::Stopping { .. } => "stopping".to_string(),
            Self::Running { .. } => "running".to_string(),
        }
    }
}

/// 集中校验状态转换
fn transition(current: &mut Option<Phase>, next: Option<Phase>) -> Result<()> {

    let from = &current;
    let to   = &next;

    // 校验状态转换是否合法
    let allowed = match (from, to) {
        (None, Some(Phase::Loading{..})) => true,
        (Some(Phase::Loading{..}), None | Some(Phase::Completed | Phase::Ready{..} | Phase::Failed{..})) => true,
        (Some(Phase::Completed), None) => true,
        (Some(Phase::Ready{..}), None | Some(Phase::Running{..})) => true,
        (Some(Phase::Failed{..}), None | Some(Phase::Loading{..})) => true,
        (Some(Phase::Running{..}), Some(Phase::Completed | Phase::Ready{..} | Phase::Failed{..} | Phase::Stopping{..})) => true,
        (Some(Phase::Stopping{..}), None | Some(Phase::Completed | Phase::Ready{..} | Phase::Failed{..})) => true,
        _ => false,
    };

    ensure!(allowed, "illegal cache transition: {} -> {}",
        from.as_ref().map_or("None".to_string(), |p| p.kind()),
        to.as_ref().map_or("None".to_string(), |p| p.kind())
    );

    // 离开 Stopping 或执行 End 前必须回收全部写入者.
    if to.is_none() || matches!((from.as_ref(), to.as_ref()),
        (Some(Phase::Stopping { .. }), Some(Phase::Ready { .. } | Phase::Completed))) {
        match from.as_ref() {
            Some(Phase::Loading { started: true, cancel } | Phase::Stopping { cancel }) => {
                ensure!(cancel.receiver_count() == 0, "cannot end a cache instance with a live worker")
            }
            _ => {},
        }
    }

    // 更新当前阶段为下一个阶段
    *current = next;
    Ok(())
}
