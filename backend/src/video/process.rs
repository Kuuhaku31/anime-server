use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{process::Command, sync::Semaphore, time::timeout};

#[derive(Clone)]
pub struct Processes {
    pub slots: Arc<Semaphore>,
}

impl Processes {
    pub fn new(total: usize) -> Self {
        Self { slots: Arc::new(Semaphore::new(total)) }
    }

    pub async fn output(&self, command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
        let _permit = timeout(Duration::from_secs(seconds), self.slots.acquire()).await
            .context("timed out waiting for a media process slot")??;
        checked_output(command, seconds).await
    }
}

pub async fn checked_output(command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
    command.kill_on_drop(true).stdin(Stdio::null());
    let output = timeout(Duration::from_secs(seconds), command.output()).await
        .context("media process timed out")?
        .context("failed to execute media process")?;
    if !output.status.success() {
        bail!("media process exited with {}: {}", output.status,
            String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4000)..]));
    }
    Ok(output.stdout)
}
