//! 统一管理媒体子进程、资源限流与执行流水线.

use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{watch, OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use tracing::info;

pub(crate) mod assets;
pub(crate) mod ffmpeg;
pub(crate) mod fragment;
pub(crate) mod models;
#[cfg(test)]
#[path = "tests/fixtures/process.rs"]
pub(crate) mod testing;
#[cfg(test)]
#[path = "tests/process.rs"]
mod tests;

pub(crate) use assets::{clean_startup, RenderAssets};
pub(crate) use ffmpeg::{
    atomic_file, command, has_video_after, media_filename, read_playlist, recover,
    regular_bytes, segment_name, sync_directory, validate_segment, write_playlist, CachedMedia,
};
pub(crate) use fragment::{normalize, Init, Tail};
pub(crate) use models::EncodeSpec;

/// 进程或线程资源的使用快照.
// 预留给 API 资源监视, 当前尚未开放 HTTP 路由.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ResourceUsage {
    pub(crate) total: usize,
    pub(crate) used: usize,
    pub(crate) available: usize,
}

/// 媒体子进程, 编码器与线程预算的使用情况快照.
// 预留给 API 资源监视, 当前尚未开放 HTTP 路由.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProcessUsage {
    pub(crate) process_slots: ResourceUsage,
    pub(crate) encoder_slots: ResourceUsage,
    pub(crate) thread_permits: ResourceUsage,
}

// 正常停止有独立错误类型, 不能顺带吞掉同一时刻发生的磁盘或清理错误.
#[derive(Debug)]
pub(crate) struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("media operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// 检查取消信号, 并在已收到取消时返回 Cancelled 错误.
pub(crate) fn check_cancel(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() { return Err(Cancelled.into()); }
    Ok(())
}

/// 发送端消失也视为取消, 避免所有者退出后子任务永久等待.
pub(crate) async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() { break; }
    }
}

/// 媒体任务的共享资源管理器, 用来控制 FFmpeg 和 ffprobe 的并发量与资源预算.
#[derive(Clone)]
pub(crate) struct ProcessManager {
    pub(crate) slots: Arc<Semaphore>, // 媒体子进程总数
    pub(crate) encoders: Arc<Semaphore>, // 并行编码任务数, 不能用线程许可替代进程名额
    threads: Arc<Semaphore>, // 编码线程预算, 不能用进程名额替代线程许可
    #[allow(dead_code)] // 仅供 usage 快照计算总量
    total_slots: usize,
    #[allow(dead_code)]
    total_encoders: usize,
    #[allow(dead_code)]
    total_threads: usize,
}

impl ProcessManager {
    pub(crate) fn new(total: usize, thread_budget: usize) -> Self {
        let total_encoders = total.saturating_sub(1);
        Self {
            slots: Arc::new(Semaphore::new(total)),
            // 至少空出一个媒体名额, 避免编码进程全部等待各自的解码校验.
            encoders: Arc::new(Semaphore::new(total_encoders)),
            threads: Arc::new(Semaphore::new(thread_budget)),
            total_slots: total,
            total_encoders,
            total_threads: thread_budget,
        }
    }

    /// 获取只读使用量快照.
    // 预留给 API 资源监视, 当前尚未开放 HTTP 路由.
    #[allow(dead_code)]
    pub(crate) fn usage(&self) -> ProcessUsage {
        let available_slots = self.slots.available_permits();
        let available_encoders = self.encoders.available_permits();
        let available_threads = self.threads.available_permits();
        ProcessUsage {
            process_slots: ResourceUsage {
                total: self.total_slots,
                used: self.total_slots.saturating_sub(available_slots),
                available: available_slots,
            },
            encoder_slots: ResourceUsage {
                total: self.total_encoders,
                used: self.total_encoders.saturating_sub(available_encoders),
                available: available_encoders,
            },
            thread_permits: ResourceUsage {
                total: self.total_threads,
                used: self.total_threads.saturating_sub(available_threads),
                available: available_threads,
            },
        }
    }

    pub(crate) async fn encoding_threads(&self, base: usize, desired: usize,
        cancel: &mut watch::Receiver<bool>) -> Result<OwnedSemaphorePermit> {
        anyhow::ensure!(base > 0 && desired >= base && desired <= 32, "invalid encoder thread request");
        // 一次取得基础预算, 再非阻塞补充许可, 不持有不足以启动的部分基础预算.
        let mut permit = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = self.threads.clone().acquire_many_owned(base as u32) => permit?,
        };
        self.extend_encoding_threads(&mut permit, desired)?;
        Ok(permit)
    }

    /// 只拿当前空闲的额外预算, 不为追求最大线程数阻塞或抢占其他编码任务.
    pub(crate) fn extend_encoding_threads(&self, permit: &mut OwnedSemaphorePermit, desired: usize) -> Result<()> {
        anyhow::ensure!(desired >= permit.num_permits() && desired <= 32, "invalid encoder thread target");
        for _ in permit.num_permits()..desired {
            match self.threads.clone().try_acquire_owned() {
                Ok(extra) => permit.merge(extra),
                Err(tokio::sync::TryAcquireError::NoPermits) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) async fn output(&self, command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
        let _permit = timeout(Duration::from_secs(seconds), self.slots.acquire())
            .await
            .context("timed out waiting for a media process slot")??;
        checked_output(command, seconds).await
    }
}

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
