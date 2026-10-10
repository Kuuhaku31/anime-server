//! 管理媒体子进程的限流和生命周期.

use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{watch, OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

// 正常停止有独立错误类型, 不能顺带吞掉同一时刻发生的磁盘或清理错误.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("media operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub fn check_cancel(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() { return Err(Cancelled.into()); }
    Ok(())
}

/// 进程总数, 并行编码数和编码线程预算分开限流, 不能用线程许可替代进程名额.
#[derive(Clone)]
pub struct Processes {
    pub slots: Arc<Semaphore>,
    pub encoders: Arc<Semaphore>,
    threads: Arc<Semaphore>,
}

impl Processes {
    pub fn new(total: usize, thread_budget: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(total)),
            // 至少空出一个媒体名额, 避免编码进程全部等待各自的解码校验.
            encoders: Arc::new(Semaphore::new(total.saturating_sub(1))),
            threads: Arc::new(Semaphore::new(thread_budget)),
        }
    }

    pub async fn encoding_threads(&self, base: usize, desired: usize,
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
    pub fn extend_encoding_threads(&self, permit: &mut OwnedSemaphorePermit, desired: usize) -> Result<()> {
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

    pub async fn output(&self, command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
        let _permit = timeout(Duration::from_secs(seconds), self.slots.acquire())
            .await
            .context("timed out waiting for a media process slot")??;
        checked_output(command, seconds).await
    }
}

pub async fn checked_output(command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
    let (_sender, mut cancel) = watch::channel(false);
    checked_output_cancellable(command, seconds, &mut cancel).await
}

pub async fn checked_output_cancellable(
    command: &mut Command,
    seconds: u64,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
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

/// 发送端消失也视为取消, 避免所有者退出后子任务永久等待.
pub async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() { break; }
    }
}

/// 持续排空 stderr 但只保留末尾诊断, 防止长时间转码累积无界日志内存.
pub async fn read_tail(mut reader: impl AsyncRead + Unpin) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stderr_retains_only_a_bounded_tail() {
        let mut input = vec![b'a'; 6000];
        input.extend_from_slice(b"last error");
        let tail = read_tail(input.as_slice()).await.unwrap();
        assert_eq!(tail.len(), 4000);
        assert!(tail.ends_with("last error"));
    }

    #[tokio::test]
    async fn weighted_threads_start_at_base_and_return_every_permit() {
        // 多个任务不能因分别占有半份基础预算而互相等待.
        let processes = Processes::new(3, 10);
        let (_sender, mut cancel) = watch::channel(false);
        let fast = processes.encoding_threads(2, 8, &mut cancel).await.unwrap();
        assert_eq!(fast.num_permits(), 8);
        let base = processes.encoding_threads(2, 8, &mut cancel).await.unwrap();
        assert_eq!(base.num_permits(), 2);
        let (stop, mut cancelled_waiter) = watch::channel(false);
        stop.send_replace(true);
        assert!(processes.encoding_threads(2, 8, &mut cancelled_waiter).await.is_err());
        drop(fast);
        drop(base);
        assert_eq!(processes.threads.available_permits(), 10);
        assert_eq!(processes.encoders.available_permits(), 2);
    }
}
