
// backend/src/tests/process.rs
//! 验证共享进程许可, 排队取消和长任务回收.

use super::*;

/// 编码预算不足时应保持排队, 取消后不得占住部分线程许可.
#[tokio::test]
async fn queued_encoder_budget_cancels_without_leaking_permits() -> Result<()> {
    let processes = ProcessManager::new(2, 2);
    let (sender, mut cancel) = watch::channel(false);
    let occupied = processes.encoding_budget(2, 2, &mut cancel).await?;
    let waiting = processes.encoding_budget(1, 1, &mut cancel);
    tokio::pin!(waiting);
    assert!(timeout(Duration::from_millis(20), &mut waiting).await.is_err());
    sender.send_replace(true);
    assert!(waiting.await.unwrap_err().is::<Cancelled>());
    drop(occupied);
    assert_eq!(processes.thread_permits.available_permits(), 2);
    Ok(())
}

/// 显式停止长任务后必须 wait 回收, 并归还三类资源许可.
#[tokio::test]
async fn worker_reaping_restores_all_resource_budgets() -> Result<()> {
    let processes = ProcessManager::new(2, 2);
    let (_sender, mut cancel) = watch::channel(false);
    let mut command = Command::new("sleep");
    command.arg("30").kill_on_drop(true).stderr(Stdio::piped()).stdout(Stdio::null());
    let usage = Arc::new(WorkerUsage::default());
    let worker = processes.start_encoder(&mut command, 2, 10, &mut cancel, usage.clone()).await?;
    assert_eq!(usage.snapshot(), WorkerResources { encoder_slots: 1, process_slots: 1, ffmpeg_threads: 2 });
    assert_eq!(processes.process_slots.available_permits(), 1);
    assert!(worker.finish(Err(Cancelled.into())).await.unwrap_err().is::<Cancelled>());
    assert_eq!(processes.process_slots.available_permits(), 2);
    assert_eq!(processes.encoder_slots.available_permits(), 1);
    assert_eq!(processes.thread_permits.available_permits(), 2);
    assert_eq!(usage.snapshot(), WorkerResources::default());
    Ok(())
}

/// 排队取消和子进程启动失败都不能留下公开占用或泄露实际许可.
#[tokio::test]
async fn cancelled_or_failed_start_keeps_worker_usage_empty() -> Result<()> {
    let processes = ProcessManager::new(2, 2);
    let (sender, mut cancel) = watch::channel(false);
    let occupied = processes.encoding_budget(2, 2, &mut cancel).await?;
    let usage = Arc::new(WorkerUsage::default());
    let mut command = Command::new("/nonexistent-anime-server-test-command");
    {
        let waiting = processes.start_encoder(&mut command, 2, 10, &mut cancel, usage.clone());
        tokio::pin!(waiting);
        assert!(timeout(Duration::from_millis(20), &mut waiting).await.is_err());
        assert_eq!(usage.snapshot(), WorkerResources::default());
        sender.send_replace(true);
        assert!(waiting.await.err().unwrap().is::<Cancelled>());
    }
    drop(occupied);
    sender.send_replace(false);
    assert!(processes.start_encoder(&mut command, 2, 10, &mut cancel, usage.clone()).await.is_err());
    assert_eq!(usage.snapshot(), WorkerResources::default());
    assert_eq!(processes.process_slots.available_permits(), 2);
    assert_eq!(processes.encoder_slots.available_permits(), 1);
    assert_eq!(processes.thread_permits.available_permits(), 2);
    Ok(())
}
