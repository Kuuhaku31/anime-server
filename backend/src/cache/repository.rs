

use std::{
    collections::HashMap,
    fs::File, future::Future, path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Arc, Weak},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use tokio::{process::Child, sync::{watch, Mutex, Notify, OwnedSemaphorePermit}, task::JoinSet, time::interval};
use tracing::{info, warn};

use sqlx::SqlitePool;

use crate::{cache::{
    CacheManager, CacheSnapshot, FolderPhase,
    FolderState, metadata::MetadataEntry, persist_used},
    config::PlaybackConfig, models::EncodeSpec, utils
};


use crate::process::{
    self, cancelled, check_cancel, read_tail, Cancelled, CachedMedia,
    Init, ProcessManager, RenderAssets, Tail,
};



// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

impl CacheManager {
    pub(super)  async fn spawn_load(&self, output: &Arc<CacheFolder>) {
        let manager = self.clone();
        let output = output.clone();
        let processes = self.inner.processes.clone();
        self.inner.workers.lock().await.spawn(async move {
            manager.load(&processes, &output).await;
            drop(output);
            if let Err(error) = manager.maintain().await { warn!("cache maintenance failed: {error:#}"); }
        });
    }

    pub(super) async fn spawn_generation(&self, output: &Arc<CacheFolder>) {
        let manager = self.clone();
        let output = output.clone();
        let processes = self.inner.processes.clone();
        self.inner.workers.lock().await.spawn(async move {
            manager.generate(&processes, &output).await;
            drop(output);
            if let Err(error) = manager.maintain().await { warn!("cache maintenance failed: {error:#}"); }
        });
    }

    pub(super) async fn get(&self, variant_key: &str) -> Result<Arc<CacheFolder>> {
        self.inner.entries.lock().await.get(variant_key).cloned().context("playback cache is no longer active")
    }

    pub(super) async fn load(&self, processes: &ProcessManager, output: &Arc<CacheFolder>) {
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

    pub(super) async fn generate(&self, processes: &ProcessManager, output: &Arc<CacheFolder>) {
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
    pub(super) async fn recover(&self, processes: &ProcessManager, output: &Arc<CacheFolder>, cancel: &mut watch::Receiver<bool>) -> Result<CachedMedia> {
        match tokio::fs::create_dir(&output.directory).await {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                ensure!(tokio::fs::symlink_metadata(&output.directory).await?.is_dir(), "unsafe cache directory"),
            Err(error) => return Err(error.into()),
        }
        let media = process::recover(&output.directory, processes, cancel).await?;
        check_cancel(cancel)?;
        Ok(media)
    }

    pub(super) async fn run_generation(&self, processes: &ProcessManager, output: &Arc<CacheFolder>, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let (resume, complete) = {
            let state = output.state.lock().await;
            (state.media.tail(), state.media.complete)
        };
        if complete { return Ok(()); }
        check_cancel(cancel)?;
        if resume.video > 0 && !process::has_video_after(&output.spec, resume, processes, cancel).await? {
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

    pub(super) async fn render(&self, processes: &ProcessManager, output: &Arc<CacheFolder>, directory: &Path, assets: Option<&RenderAssets>,
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
            let child = process::command(&output.spec, directory, assets, resume, threads.num_permits())?
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

    pub(super) async fn check_worker(&self, output: &CacheFolder, child: &mut Child,
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
    pub(super) async fn checked_step<T>(&self, output: &CacheFolder, child: &mut Child, budget: &mut EncoderBudget, started: Instant,
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
    pub(super) async fn monitor(&self, processes: &ProcessManager, output: &CacheFolder, directory: &Path, resume: Tail,
        child: &mut Child, budget: &mut EncoderBudget, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let started = Instant::now();
        let mut published = 0;
        let mut tick = interval(Duration::from_millis(100));
        loop {
            self.check_worker(output, child, budget, started, published, cancel).await?;
            // 先确认进程退出再读最终清单, 不能拿校验前的旧快照误判缺少 ENDLIST.
            let finished = child.try_wait()?.is_some_and(|exit| exit.success());
            let playlist = process::read_playlist(directory).await?;
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
    pub(super) async fn publish(&self, processes: &ProcessManager, output: &CacheFolder, directory: &Path, source_name: &str, resume: Tail,
        first: bool, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let init_bytes = process::regular_bytes(&directory.join("init.mp4"), 4 * 1024 * 1024).await?;
        let init = Init::parse(&init_bytes)?;
        ensure!(init.audio() == output.spec.config.audio_stream().is_some(), "unexpected generated audio track");
        let (expected, count) = {
            let mut state = output.state.lock().await;
            if let Some(existing) = &state.media.init { existing.compatible(&init)?; }
            else {
                process::atomic_file(&output.directory, "init.mp4", &init_bytes).await?;
                state.media.init = Some(init.clone());
            }
            (state.media.tail(), state.media.segments.len())
        };
        ensure!(count < 1_000_000, "too many cached HLS segments");
        let data = process::regular_bytes(&directory.join(source_name), 256 * 1024 * 1024).await?;
        let (data, fragment) = process::normalize(&data, &init, resume, expected, count as u32 + 1)?;
        process::atomic_file(directory, "ready.m4s", &data).await?;
        // 与正式 init 联合解码, 不能只证明分片配合本次临时 init 能播放.
        process::validate_segment(directory, "../init.mp4", "ready.m4s", init.audio(), first,
            processes, cancel).await?;
        let mut state = output.state.lock().await;
        check_cancel(cancel)?;
        let mut media = state.media.clone();
        media.segments.push(fragment);
        media.playlist()?;
        let path = output.directory.join(process::segment_name(count));
        ensure!(!tokio::fs::try_exists(&path).await?, "refusing to overwrite a published cache segment");
        // 先落盘分片再替换清单, 最后推进内存末端, 不向客户端暴露尚不存在的 URI.
        tokio::fs::rename(directory.join("ready.m4s"), path).await?;
        process::sync_directory(&output.directory).await?;
        process::write_playlist(&output.directory, &media).await?;
        state.media = media;
        // 已提交的原始临时分片不再占用双份空间, 正式分片今后只读.
        tokio::fs::remove_file(directory.join(source_name)).await?;
        Ok(())
    }

    // 先提交 ENDLIST 再写空标记, 中途崩溃可由恢复流程重新确认完整性.
    pub(super) async fn complete(&self, output: &CacheFolder) -> Result<()> {
        let mut state = output.state.lock().await;
        ensure!(!state.media.segments.is_empty(), "cannot complete an empty playback cache");
        let mut media = state.media.clone();
        media.complete = true;
        process::write_playlist(&output.directory, &media).await?;
        process::atomic_file(&output.directory, "complete", b"").await?;
        state.media = media;
        state.phase = FolderPhase::Complete;
        state.revision = REVISION.fetch_add(1, Ordering::Relaxed);
        output.changed.notify_waiters();
        info!(variant_key = output.variant_key, "playback cache complete");
        Ok(())
    }

}

// 私有共享状态只由缓存管理器和生成任务访问.
pub(super) struct CacheInner {
    pub(super) root: PathBuf,
    pub(super) config: PlaybackConfig,
    pub(super) database: SqlitePool,
    pub(super) processes: ProcessManager,
    pub(super) metadata: Mutex<HashMap<String, MetadataEntry>>,
    // 同时访问集合和元素时先锁 entries 再锁 state, 创建与回收必须保持相同顺序.
    pub(super) entries: Mutex<HashMap<String, Arc<CacheFolder>>>,
    pub(super) folders: Mutex<HashMap<String, Weak<CacheFolder>>>,
    pub(super) workers: Mutex<JoinSet<()>>,
    pub(super) maintenance: Mutex<()>,
    pub(super) stop: watch::Sender<bool>,
    pub(super) _directory_lock: File,
}

/// 同一配置的全部会话共享此元素, 只有其生成任务能发布正式分片.
pub(super) struct CacheFolder {
    pub(super) variant_key: String,
    pub(super) directory: PathBuf,
    pub(super) spec: EncodeSpec,
    pub(super) state: Mutex<FolderState>,
    pub(super) changed: Notify,
}

impl CacheFolder {
    pub(super) fn snapshot(&self, state: &FolderState) -> CacheSnapshot {
        CacheSnapshot {
            phase: state.phase.snapshot(),
            revision: state.revision,
            buffered_until: state.media.tail().seconds(),
            duration: self.spec.duration,
        }
    }

    pub(super) async fn wait_loaded(&self) -> Result<CacheSnapshot> {
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

pub(super) static REVISION: AtomicU64 = AtomicU64::new(1);

// 进程实际退出时立即归还预算, 不把后续磁盘提交算成正在编码.
pub(super) type EncoderBudget = Option<(OwnedSemaphorePermit, OwnedSemaphorePermit, OwnedSemaphorePermit)>;
