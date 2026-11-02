
// backend/src/tests/cache/handle.rs
//! 验证六状态转换矩阵和 End 的写入者约束.

use super::*;

/// 每条转换都必须符合架构矩阵, End 不能成为额外可查询状态.
#[test]
fn transition_matrix_has_only_the_six_documented_phases() {
    let kinds = [CachePhase::Loading, CachePhase::Completed, CachePhase::Ready,
        CachePhase::Failed, CachePhase::Stopping, CachePhase::Running];
    for from in kinds {
        for to in kinds.into_iter().map(Some).chain([None]) {
            let expected = match from {
                CachePhase::Loading => to.is_none() || matches!(to, Some(CachePhase::Completed | CachePhase::Ready | CachePhase::Failed)),
                CachePhase::Completed => to.is_none(),
                CachePhase::Ready => to.is_none() || to == Some(CachePhase::Running),
                CachePhase::Failed => to.is_none() || to == Some(CachePhase::Loading),
                CachePhase::Stopping => to.is_none() || to == Some(CachePhase::Failed),
                CachePhase::Running => matches!(to, Some(CachePhase::Completed | CachePhase::Ready | CachePhase::Failed | CachePhase::Stopping)),
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
    transition(&mut *handle.phase.lock().await, Some(phase(CachePhase::Ready)))?;
    handle.run(directory.path.join("missing.mkv"), crate::testing::metadata(),
        &AppConfigManager::default(), &ProcessManager::new(2, 2)).await?;
    assert!(handle.stop().await?, "run returned before registering its worker");
    assert_eq!(handle.snapshot().await?.phase, CachePhase::Stopping);
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
    assert_eq!(handle.snapshot().await?.phase, CachePhase::Ready);
    assert!(std::fs::read_dir(&directory.path)?.next().is_none());
    Ok(())
}

/// 构造没有实际 worker 的状态机样本.
fn phase(kind: CachePhase) -> Phase {
    match kind {
        CachePhase::Loading => Phase::loading(),
        CachePhase::Completed => Phase::Completed,
        CachePhase::Ready => Phase::Ready { cancel: watch::channel(false).0 },
        CachePhase::Failed => Phase::Failed { error: "test failure".to_string() },
        CachePhase::Stopping => Phase::Stopping { cancel: watch::channel(false).0 },
        CachePhase::Running => Phase::Running { cancel: watch::channel(false).0 },
    }
}
