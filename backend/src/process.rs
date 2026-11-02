
// backend/src/process.rs
//! 统一管理媒体子进程, 资源限流和执行流水线.

use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{watch, OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use tracing::info;

mod assets;
mod ffmpeg;
mod fragment;
mod metadata;
mod worker;
pub(crate) use assets::RenderAssets;
pub(crate) use ffmpeg::{
    atomic_file, media_filename, read_playlist, regular_bytes, segment_name,
    sync_directory, write_playlist, CachedMedia,
};
pub(crate) use fragment::{normalize, Init, Tail};
pub(crate) use worker::{EncodingWorker, WorkerResources, WorkerUsage};

#[cfg(test)]
#[path = "tests/process.rs"]
mod tests;

/// 区分正常取消和磁盘, 解码或进程回收失败.
#[derive(Debug)]
pub(crate) struct Cancelled;

impl std::fmt::Display for Cancelled {
    /// 提供可记录的取消原因.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("media operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// 只保存三组共享资源许可, 任务中的克隆仍使用相同预算.
#[derive(Clone)]
pub(crate) struct ProcessManager {
    /// 媒体探测, 编码和分片校验共用的进程名额.
    process_slots: Arc<Semaphore>,
    /// 编码任务名额, 至少为解码校验保留一个进程位置.
    encoder_slots: Arc<Semaphore>,
    /// 所有编码任务共享的线程预算.
    thread_permits: Arc<Semaphore>,
}

impl ProcessManager {
    /// 根据已校验的配置建立全局资源预算.
    pub(crate) fn new(total: usize, thread_budget: usize) -> Self {
        Self {
            process_slots: Arc::new(Semaphore::new(total)),
            // 至少空出一个媒体名额, 避免编码进程全部等待各自的解码校验.
            encoder_slots: Arc::new(Semaphore::new(total.saturating_sub(1))),
            thread_permits: Arc::new(Semaphore::new(thread_budget)),
        }
    }

    /// 原子取得基础线程预算, 再尝试利用空闲许可.
    pub(crate) async fn encoding_budget(&self, base: usize, desired: usize,
        cancel: &mut watch::Receiver<bool>) -> Result<OwnedSemaphorePermit> {
        anyhow::ensure!(base > 0 && desired >= base && desired <= 32, "invalid encoder thread request");
        // 一次取得基础预算, 再非阻塞补充许可, 不持有不足以启动的部分基础预算.
        let mut permit = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = self.thread_permits.clone().acquire_many_owned(base as u32) => permit?,
        };
        self.extend_encoding_budget(&mut permit, desired)?;
        Ok(permit)
    }

    /// 只拿当前空闲的额外预算, 不为追求最大线程数阻塞或抢占其他编码任务.
    pub(crate) fn extend_encoding_budget(&self, permit: &mut OwnedSemaphorePermit, desired: usize) -> Result<()> {
        anyhow::ensure!(desired >= permit.num_permits() && desired <= 32, "invalid encoder thread target");
        for _ in permit.num_permits()..desired {
            match self.thread_permits.clone().try_acquire_owned() {
                Ok(extra) => permit.merge(extra),
                Err(tokio::sync::TryAcquireError::NoPermits) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// 在统一进程名额和超时约束下执行短命令.
    pub(crate) async fn output(&self, command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
        let _permit = timeout(Duration::from_secs(seconds), self.process_slots.acquire())
            .await
            .context("timed out waiting for a media process slot")??;
        checked_output(command, seconds).await
    }
}

/// 检查取消信号, 防止已取消任务继续启动子进程或发布文件.
pub(crate) fn check_cancel(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() { return Err(Cancelled.into()); }
    Ok(())
}

/// 将发送端退出视为取消, 避免任务失去所有者后永久等待.
pub(crate) async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() { break; }
    }
}

/// 执行启动依赖检查等不属于播放任务的短命令.
pub(crate) async fn checked_output(command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
    let (_sender, mut cancel) = watch::channel(false);
    match checked_output_cancellable(command, seconds, &mut cancel).await {
        Ok(output) => Ok(output),
        Err(error) => {
            info!("media command failed: {:?}: {:#}", command, error);
            Err(error)
        },
    }
}

/// 通用的异步子进程执行器, 用它运行 ffmpeg, ffprobe 等命令.
/// 等待命令结束, 负责限时等待, 处理取消信号, 收集输出和诊断信息, 并在退出前回收子进程.
pub(super) async fn checked_output_cancellable(
    command: &mut Command,
    seconds: u64,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    info!("运行媒体命令: {:?}", command);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("failed to execute media process")?;
    let mut stdout = child.stdout.take().context("media stdout is unavailable")?;
    let stderr = child.stderr.take().context("media stderr is unavailable")?;
    // 同时读取两个管道, 避免子进程因任一管道写满而阻塞, 造成假超时.
    let reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let errors = tokio::spawn(read_tail(stderr));
    let result = tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(Cancelled.into()),
        result = timeout(Duration::from_secs(seconds), child.wait()) =>
            result.context("media process timed out").and_then(|exit| exit.context("media process wait failed")),
    };
    // 超时和取消也先终止并 wait 回收子进程, 再向调用方返回错误.
    if child.try_wait()?.is_none() {
        child.kill().await?;
    }
    child.wait().await?;
    let output = reader.await.context("media output reader failed")??;
    let tail = errors.await.context("media error reader failed")??;
    let status = result?;
    if !status.success() {
        bail!("media process exited with {status}: {tail}");
    }
    Ok(output)
}

/// 持续排空 stderr 但只保留末尾诊断, 防止长时间转码累积无界日志内存.
pub(crate) async fn read_tail(mut reader: impl AsyncRead + Unpin) -> Result<String> {
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 4000 {
            tail.drain(..tail.len() - 4000);
        }
    }
    Ok(String::from_utf8_lossy(&tail).into_owned())
}
