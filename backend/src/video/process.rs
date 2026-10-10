//! 管理媒体子进程的限流和生命周期.

use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{watch, Semaphore},
    time::timeout,
};

#[derive(Clone)]
pub struct Processes {
    pub slots: Arc<Semaphore>,
}

impl Processes {
    pub fn new(total: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(total)),
        }
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
    let reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let errors = tokio::spawn(read_tail(stderr));
    let result = tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(anyhow::anyhow!("media process cancelled")),
        result = timeout(Duration::from_secs(seconds), child.wait()) =>
            result.context("media process timed out").and_then(|exit| exit.context("media process wait failed")),
    };
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

pub async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    if !*receiver.borrow() {
        let _ = receiver.changed().await;
    }
}

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
}
