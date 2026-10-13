//! 校验播放会话线程策略, 目标位置, 过期回收和共享缓存租约的测试.

use std::time::{Duration, Instant};

use crate::{
    cache::{testing, CacheManager},
    config::PlaybackConfig,
    process::{testing::FILE_ID, ProcessManager},
    models::PlaybackRequest,
};

use super::*;

// 等于缓存尾部时仍需等待后续分片, 因此选择变体线程上限.
#[test]
fn thread_policy_uses_maximum_when_position_reaches_cache_tail() {
    assert_eq!(select_threads(6.0, 5.0, 2, 8), 8);
    assert_eq!(select_threads(5.0, 5.0, 2, 8), 8);
    assert_eq!(select_threads(4.0, 5.0, 2, 8), 2);
}

// 非法或越界的目标位置必须在预留会话和激活缓存之前被拒绝.
#[tokio::test]
async fn invalid_positions_fail_before_reserving_a_session() {
    let (cache, root) = cache_fixture(ProcessManager::new(2, 16)).await;
    let manager = SessionManager::new(60);
    for position in [-1.0, f64::NAN, f64::INFINITY, 20.0] {
        let mut request = PlaybackRequest::default();
        request.position = Some(position);
        assert!(manager.create(&cache, FILE_ID, request).await.is_err());
    }
    let saved = PlaybackRequest::default().with_saved_position(Some(-1.0));
    assert!(manager.create(&cache, FILE_ID, saved).await.is_err());
    assert!(manager.sessions.lock().await.is_empty());
    assert!(cache.active_variants().await.is_empty());
    cache.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 创建预留阶段享有更长宽限, 但被中断的请求不能永久占住会话名额.
#[tokio::test]
async fn an_abandoned_creation_reservation_expires_after_its_grace_period() {
    let (cache, root) = cache_fixture(ProcessManager::new(2, 16)).await;
    let manager = SessionManager::new(20);
    {
        let mut sessions = manager.sessions.lock().await;
        for (session_id, seconds) in [("loading", 30), ("abandoned", 121)] {
            sessions.insert(session_id.to_string(), Session {
                variant_key: "a".repeat(32), target: 0.0, touched: None,
                created: Instant::now() - Duration::from_secs(seconds),
            });
        }
    }
    expire(&manager, &cache).await.unwrap();
    let sessions = manager.sessions.lock().await;
    assert!(sessions.contains_key("loading"));
    assert!(!sessions.contains_key("abandoned"));
    drop(sessions);
    manager.shutdown().await;
    cache.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 同配置会话共享一个缓存条目, 最后一个会话过期时取消仍在排队的生成任务.
#[tokio::test]
async fn sessions_share_one_cache_and_last_expiry_cancels_queued_work() {
    // 先占满媒体名额, 让生成任务停在排队阶段, 不能意外启动编码.
    let processes = ProcessManager::new(2, 16);
    let held = processes.slots.clone().acquire_many_owned(2).await.unwrap();
    let (cache, root) = cache_fixture(processes).await;
    let manager = SessionManager::new(60);
    let first = manager.create(&cache, FILE_ID, PlaybackRequest::default()).await.unwrap();
    let second = manager.create(&cache, FILE_ID, PlaybackRequest::default().with_saved_position(Some(19.0)))
        .await.unwrap();
    // 从响应 JSON 读取会话 ID, 验证 flatten 结构和实际选轨字段.
    let first = serde_json::to_value(first).unwrap();
    let second = serde_json::to_value(second).unwrap();
    assert!(first.get("session").is_none());
    assert!(first["media"].is_object());
    assert_eq!(first["selection"], serde_json::json!({"audio": 1, "subtitle": null}));
    assert_eq!(first["phase"], "running");
    // 接近结尾的历史进度视为已看完, 新会话从头开始.
    assert_eq!(second["position"], 0.0);
    let first = first["session_id"].as_str().unwrap().to_owned();
    let second = second["session_id"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    assert_eq!(cache.active_variants().await.len(), 1);

    manager.release(&cache, &first).await.unwrap();
    manager.release(&cache, &first).await.unwrap();
    assert!(manager.status(&cache, &first).await.is_err());
    assert_eq!(cache.active_variants().await.len(), 1);
    let status = serde_json::to_value(manager.target(&cache, &second, 7.375).await.unwrap()).unwrap();
    assert_eq!(status["position"], 7.375);
    assert!(manager.target(&cache, &second, 20.0).await.is_err());

    manager.sessions.lock().await.get_mut(&second).unwrap().touched = Some(Instant::now() - Duration::from_secs(61));
    expire(&manager, &cache).await.unwrap();
    assert!(manager.status(&cache, &second).await.is_err());
    tokio::time::timeout(Duration::from_secs(3), async {
        while !cache.active_variants().await.is_empty() { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
    assert!(manager.media_path(&cache, &second, "init.mp4").await.is_err());
    manager.shutdown().await;
    cache.shutdown().await.unwrap();
    drop(held);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// ========================================================================== //

// 创建带有已登记测试媒体的缓存管理器, 媒体信息来自预置元数据.
async fn cache_fixture(processes: ProcessManager) -> (CacheManager, std::path::PathBuf) {
    let (cache, root) = testing::manager(PlaybackConfig::default(), processes).await;
    testing::register_media(&cache, FILE_ID).await;
    (cache, root)
}
