
// backend/src/process/worker.rs
//! 管理长时间编码进程的启动, 监控和确定性回收.

use std::time::Instant;

use anyhow::{ensure, Context, Result};
use tokio::{process::{Child, Command}, sync::{watch, OwnedSemaphorePermit}, task::JoinHandle};

use super::{cancelled, check_cancel, read_tail, Cancelled, ProcessManager};

/// 保存单个编码进程及其生命周期内占用的许可.
pub(crate) struct EncodingWorker {
    /// 当前 FFmpeg 子进程.
    child: Child,
    /// 持续排空 stderr 并保存末尾诊断的任务.
    errors: JoinHandle<Result<String>>,
    /// 进程存活期间持有的编码器, 进程和线程许可.
    budget: Option<(OwnedSemaphorePermit, OwnedSemaphorePermit, OwnedSemaphorePermit)>,
    /// 实际启动进程的时刻, 不包含排队等待时间.
    started: Instant,
    /// 首个可发布分片的最长等待秒数.
    startup_seconds: u64,
}

impl ProcessManager {
    /// 取得全部编码许可后启动命令, 取消时不留下排队任务或子进程.
    pub(crate) async fn start_encoder(
        &self, command: &mut Command, threads: usize, startup_seconds: u64,
        cancel: &mut watch::Receiver<bool>,
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
        tracing::info!(pid = child.id(), threads, "media encoder started");
        Ok(EncodingWorker {
            child, errors: tokio::spawn(read_tail(stderr)),
            budget: Some((encoder, process, thread_budget)),
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
