
// backend/src/process/worker.rs
//! 管理长时间编码进程的启动, 监控和确定性回收.

use std::{sync::{atomic::{AtomicUsize, Ordering}, Arc}, time::Instant};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::{process::{Child, Command}, sync::{watch, OwnedSemaphorePermit}, task::JoinHandle};

use super::{cancelled, check_cancel, read_tail, Cancelled, ProcessManager};

/// 描述已启动转码 worker 当前持有的三类资源.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerResources {
    /// 长时间编码进程名额, 单个 worker 为 0 或 1.
    pub(crate) encoder_slots: usize,
    /// 转码进程占用的媒体名额, 不包含短时校验进程.
    pub(crate) process_slots: usize,
    /// 当前持有的 FFmpeg 编码线程预算.
    pub(crate) ffmpeg_threads: usize,
}

/// 使用单个原子值同时发布一个 worker 的完整资源快照.
#[derive(Debug, Default)]
pub(crate) struct WorkerUsage {
    /// 零表示未启动, 非零表示已启动 worker 持有的线程预算.
    threads: AtomicUsize,
}

impl WorkerUsage {
    /// 获取不持有业务状态锁的资源快照.
    pub(crate) fn snapshot(&self) -> WorkerResources {
        let threads = self.threads.load(Ordering::Acquire);
        WorkerResources {
            encoder_slots: usize::from(threads > 0),
            process_slots: usize::from(threads > 0),
            ffmpeg_threads: threads,
        }
    }
}

/// 保存单个编码进程及其生命周期内占用的许可.
pub(crate) struct EncodingWorker {
    /// 当前 FFmpeg 子进程.
    child: Child,
    /// 持续排空 stderr 并保存末尾诊断的任务.
    errors: JoinHandle<Result<String>>,
    /// 进程存活期间持有的编码器, 进程和线程许可.
    budget: Option<WorkerBudget>,
    /// 实际启动进程的时刻, 不包含排队等待时间.
    started: Instant,
    /// 首个可发布分片的最长等待秒数.
    startup_seconds: u64,
}

impl ProcessManager {
    /// 取得全部编码许可后启动命令, 取消时不留下排队任务或子进程.
    pub(crate) async fn start_encoder(
        &self, command: &mut Command, threads: usize, startup_seconds: u64,
        cancel: &mut watch::Receiver<bool>, usage: Arc<WorkerUsage>,
    ) -> Result<EncodingWorker> {
        let encoder = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = self.encoder_slots.clone().acquire_owned() => permit?,
        };
        let thread_budget = self.encoding_budget(threads, threads, cancel).await?;
        let process = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = self.process_slots.clone().acquire_owned() => permit?,
        };
        check_cancel(cancel)?;
        let mut child = command.spawn().context("failed to start FFmpeg")?;
        let stderr = child.stderr.take().context("FFmpeg stderr is unavailable")?;
        usage.threads.store(thread_budget.num_permits(), Ordering::Release);
        tracing::info!(pid = child.id(), threads, "media encoder started");
        Ok(EncodingWorker {
            child, errors: tokio::spawn(read_tail(stderr)),
            budget: Some(WorkerBudget { _permits: (encoder, process, thread_budget), usage }),
            started: Instant::now(), startup_seconds,
        })
    }

}

impl EncodingWorker {
    /// 检查退出状态和首片超时, 退出后立即归还许可而不等待后续磁盘发布.
    pub(crate) fn check(&mut self, published: usize) -> Result<bool> {
        let exit = self.child.try_wait()?;
        if let Some(exit) = exit {
            self.budget.take();
            ensure!(exit.success(), "FFmpeg exited with {exit}");
        }
        ensure!(published > 0 || self.started.elapsed().as_secs() < self.startup_seconds,
            "FFmpeg startup timed out");
        Ok(exit.is_some())
    }

    /// 请求精确终止当前进程, 最终由 finish 等待回收.
    pub(crate) fn interrupt(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() { self.child.start_kill()?; }
        Ok(())
    }

    /// 无论执行结果如何都先回收子进程和日志任务, 再返回诊断.
    pub(crate) async fn finish(mut self, result: Result<()>) -> Result<()> {
        let cleanup = async {
            if self.child.try_wait()?.is_none() { self.child.kill().await?; }
            self.child.wait().await?;
            Ok::<_, std::io::Error>(())
        }.await;
        self.budget.take();
        let tail = self.errors.await.context("FFmpeg error reader failed")??;
        cleanup.context("failed to reap FFmpeg")?;
        result.with_context(|| if tail.is_empty() { "FFmpeg generation ended".to_string() }
            else { format!("FFmpeg: {tail}") })
    }
}

/// 将实际许可与公开计数绑定到同一释放作用域.
struct WorkerBudget {
    /// 生命周期内实际占用的编码, 进程和线程许可.
    _permits: (OwnedSemaphorePermit, OwnedSemaphorePermit, OwnedSemaphorePermit),
    /// 对应句柄的资源记录器, 不持有管理器或缓存句柄.
    usage: Arc<WorkerUsage>,
}

impl Drop for WorkerBudget {
    /// 正常退出, 错误或任务取消时均清空公开资源计数.
    fn drop(&mut self) {
        self.usage.threads.store(0, Ordering::Release);
    }
}
