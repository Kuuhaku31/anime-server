
// backend/tests/cache/handle.rs
//! 验证六状态转换矩阵和 End 的写入者约束.

use super::*;

/// 每条转换都必须符合架构矩阵, End 不能成为额外可查询状态.
#[test]
fn transition_matrix_has_only_the_six_documented_phases() {
    let kinds = ["loading", "completed", "ready", "failed", "stopping", "running"];
    for from in kinds {
        for to in kinds.into_iter().map(Some).chain([None]) {
            let expected = match from {
                "loading" => to.is_none() || matches!(to, Some("completed" | "ready" | "failed")),
                "completed" => to.is_none(),
                "ready" => to.is_none() || to == Some("running"),
                "failed" => to.is_none() || to == Some("loading"),
                "stopping" => to.is_none() || matches!(to, Some("completed" | "ready" | "failed")),
                "running" => matches!(to, Some("completed" | "ready" | "failed" | "stopping")),
                _ => unreachable!("unknown test phase: {from}"),
            };
            let mut current = Some(phase(from));
            assert_eq!(transition(&mut current, to.map(phase)).is_ok(), expected, "{from:?} -> {to:?}");
        }
    }
}

/// 活动恢复或停止任务退出前不能执行 End.
#[test]
fn end_rejects_a_live_recovery_worker() {
    let (cancel, _receiver) = watch::channel(false);
    let mut current = Some(Phase::Loading { started: true, cancel });
    assert!(transition(&mut current, None).is_err());
}

/// run 返回前必须登记写入者, 不能在 stop 返回后才偷偷启动生成.
#[tokio::test]
async fn ready_run_reserves_its_worker_before_stop() -> Result<()> {
    let directory = crate::testing::TestDirectory::new();
    let spec = crate::testing::spec();
    let handle = Arc::new(CacheInstanceHandle::new(spec.clone(), spec.handle_id()?, directory.path.clone()));
    transition(&mut *handle.phase.lock().await, Some(phase("ready")))?;
    handle.run(directory.path.join("missing.mkv"), crate::testing::metadata(),
        &AppConfigManager::default(), &ProcessManager::new(2, 2)).await?;
    assert!(handle.stop().await?, "run returned before registering its worker");
    assert_eq!(handle.snapshot().await?.status.phase, "stopping");
    Ok(())
}

/// Loading 刚转入 Ready 时停止, 必须取消尚未开始运行的续接请求.
#[tokio::test]
async fn stop_cancels_a_deferred_run_after_recovery_becomes_ready() -> Result<()> {
    let directory = crate::testing::TestDirectory::new();
    let spec = crate::testing::spec();
    let handle = Arc::new(CacheInstanceHandle::new(spec.clone(), spec.handle_id()?, directory.path.clone()));
    let cancel = match handle.phase.lock().await.as_ref().unwrap() {
        Phase::Loading { cancel, .. } => cancel.clone(),
        _ => unreachable!(),
    };
    handle.run(directory.path.join("missing.mkv"), crate::testing::metadata(),
        &AppConfigManager::default(), &ProcessManager::new(2, 2)).await?;
    transition(&mut *handle.phase.lock().await, Some(Phase::Ready { cancel }))?;
    assert!(!handle.stop().await?);
    assert_eq!(handle.snapshot().await?.status.phase, "ready");
    assert!(std::fs::read_dir(&directory.path)?.next().is_none());
    Ok(())
}

/// 停止中的 worker 未退出前不能提前变为可续接状态.
#[test]
fn stopping_cannot_become_ready_with_a_live_worker() {
    let (cancel, receiver) = watch::channel(false);
    let mut current = Some(Phase::Stopping { cancel });
    assert!(transition(&mut current, Some(phase("ready"))).is_err());
    drop(receiver);
    assert!(transition(&mut current, Some(phase("ready"))).is_ok());
}

/// 用户停止不能取消非 Running 阶段的恢复, 也不能等待其接收端退出.
#[tokio::test]
async fn pause_does_not_cancel_loading_recovery() -> Result<()> {
    let directory = crate::testing::TestDirectory::new();
    let spec = crate::testing::spec();
    let handle = CacheInstanceHandle::new(spec.clone(), spec.handle_id()?, directory.path.clone());
    let (cancel, receiver) = watch::channel(false);
    *handle.phase.lock().await = Some(Phase::Loading { started: true, cancel });
    tokio::time::timeout(Duration::from_millis(100), handle.pause()).await??;
    assert!(!*receiver.borrow());
    assert_eq!(handle.snapshot().await?.status.phase, "loading");
    Ok(())
}

/// 正在停止转码时仍可读取已发布前缀, 但不能读取清单外的分片.
#[tokio::test]
async fn stopping_keeps_published_assets_available() -> Result<()> {
    let directory = crate::testing::TestDirectory::new();
    let spec = crate::testing::spec();
    let handle = CacheInstanceHandle::new(spec.clone(), spec.handle_id()?, directory.path.clone());
    tokio::fs::write(directory.path.join("index.m3u8"),
        "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nsegment_000000.m4s\n").await?;
    for name in ["init.mp4", "segment_000000.m4s", "segment_000001.m4s"] {
        tokio::fs::write(directory.path.join(name), b"published asset").await?;
    }
    *handle.phase.lock().await = Some(phase("stopping"));
    for name in ["index.m3u8", "init.mp4", "segment_000000.m4s"] {
        assert_eq!(handle.media_path(name).await?, directory.path.join(name));
    }
    assert!(handle.media_path("segment_000001.m4s").await.is_err());
    assert!(handle.media_path("instance.json").await.is_err());
    handle.pause().await?;
    assert_eq!(handle.snapshot().await?.status.phase, "ready");
    Ok(())
}

/// 构造没有实际 worker 的状态机样本.
fn phase(kind: &str) -> Phase {
    match kind {
        "loading" => Phase::loading(),
        "completed" => Phase::Completed,
        "ready" => Phase::Ready { cancel: watch::channel(false).0 },
        "failed" => Phase::Failed { error: "test failure".to_string() },
        "stopping" => Phase::Stopping { cancel: watch::channel(false).0 },
        "running" => Phase::Running { cancel: watch::channel(false).0 },
        _ => unreachable!("unknown test phase: {kind}"),
    }
}
