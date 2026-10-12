//! 管理不透明播放会话并协调活动缓存租约.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::{Duration, Instant}};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::{sync::Mutex, time::interval};
use tracing::warn;

use crate::{models::PlaybackStart, video::Processes};

use super::{cache::{random_id, CacheManager}, cache::ActiveCache, models::EncodeSpec};

/// 对外只暴露不透明会话和状态快照, 不返回播放配置或 variant-key.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionStatus {
    session_id: String,
    url: String,
    mime_type: &'static str,
    start_time: f64,
    pub(crate) duration: f64,
    position: f64,
    complete: bool,
    generating: bool,
    stopping: bool,
    ready: bool,
    buffered_until: f64,
    encoding_threads: usize,
    error: Option<String>,
}

// 会话只记录缓存键和存活时间, 目标位置及媒体状态由活动元素统一维护.
struct Session {
    variant_key: String,
    // 创建阶段给予额外宽限, 但被中断的 HTTP 请求不能永久占住会话名额.
    created: Instant,
    // None 表示创建预留阶段, 第一次有效访问后才启用普通心跳超时.
    touched: Option<Instant>,
}

#[derive(Clone)]
pub(crate) struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    idle_seconds: u64,
}

impl SessionManager {
    pub(crate) fn new(idle_seconds: u64) -> Self {
        Self { sessions: Arc::new(Mutex::new(HashMap::new())), idle_seconds }
    }

    /// 先预留会话名额再等待缓存加载, 防止并发创建绕过数量上限.
    pub(crate) async fn create(&self, processes: &Processes, cache: &CacheManager, spec: EncodeSpec) -> Result<PlaybackStart> {
        let id = random_id()?;
        let key = spec.config.key()?;
        let media = spec.info.clone();
        let selection = (spec.config.audio_stream, spec.config.subtitle_stream);
        {
            let mut sessions = self.sessions.lock().await;
            ensure!(!cache.stopping(), "server is shutting down");
            ensure!(sessions.len() < 32, "too many playback sessions; stop an existing stream");
            sessions.insert(id.clone(), Session { variant_key: key.clone(), created: Instant::now(), touched: None });
        }
        // 等待旧写入者回收时不持有会话表锁, 其他客户端仍能立即释放租约.
        let result = async {
            cache.acquire(processes, spec, &id).await?.wait_loaded().await?;
            self.status(cache, &id).await
        }.await;
        match result {
            Ok(session) => Ok(PlaybackStart::new(session, media, selection.0, selection.1)),
            Err(error) => {
                self.sessions.lock().await.remove(&id);
                if let Err(cleanup) = cache.release(&key, &id).await {
                    return Err(error.context(format!("session cleanup failed: {cleanup:#}")));
                }
                Err(error)
            },
        }
    }

    // 状态, 媒体和显式心跳请求都经此续期, 客户端不能自行提供缓存键.
    async fn key(&self, id: &str) -> Result<String> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id).context("playback session not found or expired")?;
        session.touched = Some(Instant::now());
        Ok(session.variant_key.clone())
    }

    pub(crate) async fn status(&self, cache: &CacheManager, id: &str) -> Result<SessionStatus> {
        let key = self.key(id).await?;
        let cache = cache.get(&key).await?;
        let status = cache.status(id).await?;
        Ok(SessionStatus {
            session_id: id.to_string(), url: format!("/playback/{id}/index.m3u8?revision={}", status.revision),
            mime_type: "application/vnd.apple.mpegurl", start_time: 0.0, duration: cache.spec.duration,
            position: status.target, complete: status.complete, generating: status.generating,
            stopping: status.stopping, ready: status.ready, buffered_until: status.buffered_until,
            encoding_threads: status.encoding_threads, error: status.error,
        })
    }

    pub(crate) async fn heartbeat(&self, cache: &CacheManager, id: &str) -> Result<()> {
        self.status(cache, id).await?;
        Ok(())
    }

    pub(crate) async fn target(&self, cache: &CacheManager, id: &str, target: f64) -> Result<SessionStatus> {
        let key = self.key(id).await?;
        cache.target(&key, id, target).await?;
        self.status(cache, id).await
    }

    pub(crate) async fn media_path(&self, cache: &CacheManager, id: &str, name: &str) -> Result<(PathBuf, &'static str, Arc<ActiveCache>)> {
        let key = self.key(id).await?;
        let (path, cache) = cache.media_path(&key, id, name).await?;
        let mime = if name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" }
            else if name.ends_with(".m4s") { "video/iso.segment" } else { "video/mp4" };
        // 只在文件打开期间持有额外 Arc, 即使最后一份租约被删除也不会被 LRU 抢先删除.
        Ok((path, mime, cache))
    }

    /// 先移除会话再释放租约, 重复释放和并发过期清理只会有一方实际扣除租约.
    pub(crate) async fn release(&self, cache: &CacheManager, id: &str) -> Result<()> {
        let session = self.sessions.lock().await.remove(id);
        if let Some(session) = session { cache.release(&session.variant_key, id).await?; }
        Ok(())
    }

    pub(crate) fn spawn_expiration(&self, cache: &CacheManager) {
        let manager = self.clone();
        let cache = cache.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if cache.stopping() { break; }
                if let Err(error) = expire(&cache, &manager).await {
                    warn!("playback cache maintenance failed: {error:#}");
                }
            }
        });
    }

    pub(crate) async fn shutdown(&self, cache: &CacheManager, processes: &Processes) -> Result<()> {
        self.sessions.lock().await.clear();
        cache.shutdown(processes).await
    }
}

// 锁内批量摘除过期会话, 锁外逐一清理, 单个失败不能阻断其余租约释放.
async fn expire(cache: &CacheManager, manager: &SessionManager) -> Result<()> {
    let expired = {
        let mut sessions = manager.sessions.lock().await;
        let ids: Vec<_> = sessions.iter().filter(|(_, session)| match session.touched {
            Some(touched) => touched.elapsed().as_secs() >= manager.idle_seconds,
            None => session.created.elapsed().as_secs() >= manager.idle_seconds.max(120),
        })
            .map(|(id, _)| id.clone()).collect();
        ids.into_iter().filter_map(|id| sessions.remove(&id).map(|session| (id, session))).collect::<Vec<_>>()
    };
    let mut errors = Vec::new();
    for (id, session) in expired {
        if let Err(error) = cache.release(&session.variant_key, &id).await {
            errors.push(format!("session {id}: {error:#}"));
        }
    }
    if let Err(error) = cache.maintain().await { errors.push(format!("{error:#}")); }
    ensure!(errors.is_empty(), "session expiration failed: {}", errors.join("; "));
    Ok(())
}


// ========================================================================== //

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::{config::PlaybackConfig, video::{fixture, Processes}};

//     #[tokio::test]
//     async fn an_abandoned_creation_reservation_expires_after_its_grace_period() {
//         let root = std::env::temp_dir().join(format!("anime-pending-lease-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let cache = CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(2, 16)).await.unwrap();
//         let manager = SessionManager::new(cache, 20);
//         {
//             let mut sessions = manager.sessions.lock().await;
//             for (id, seconds) in [("loading", 30), ("abandoned", 121)] {
//                 sessions.insert(id.to_string(), Session {
//                     variant_key: "a".repeat(32), touched: None,
//                     created: Instant::now() - Duration::from_secs(seconds),
//                 });
//             }
//         }
//         manager.expire().await.unwrap();
//         let sessions = manager.sessions.lock().await;
//         assert!(sessions.contains_key("loading"));
//         assert!(!sessions.contains_key("abandoned"));
//         drop(sessions);
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }

//     #[tokio::test]
//     async fn concurrent_sessions_share_one_entry_and_last_expiry_cancels_queued_work() {
//         // 先占满媒体名额, 让最后释放发生在排队阶段, 不能意外启动编码.
//         let root = std::env::temp_dir().join(format!("anime-active-leases-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let processes = Processes::new(2, 16);
//         let permit = processes.slots.acquire_many(2).await.unwrap();
//         let cache = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
//         let manager = SessionManager::new(cache.clone(), 60);
//         let (info, request) = fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let key = spec.config.key().unwrap();
//         let (first, second) = tokio::join!(manager.create(spec.clone()), manager.create(spec));
//         // 从响应 JSON 读取会话 ID, 验证模型迁移后 flatten 及轨道字段保持不变.
//         let first = serde_json::to_value(first.unwrap()).unwrap();
//         let second = serde_json::to_value(second.unwrap()).unwrap();
//         assert!(first.get("session").is_none());
//         assert!(first["media"].is_object());
//         assert_eq!(first["selection"], serde_json::json!({"audio": 1, "subtitle": null}));
//         let first = first["session_id"].as_str().unwrap().to_owned();
//         let second = second["session_id"].as_str().unwrap().to_owned();
//         assert_ne!(first, second);
//         let output = cache.get(&key).await.unwrap();
//         assert!(Arc::ptr_eq(&output, &cache.get(&key).await.unwrap()));
//         manager.release(&first).await.unwrap();
//         manager.release(&first).await.unwrap();
//         assert!(manager.status(&first).await.is_err());
//         assert!(manager.status(&second).await.unwrap().generating);
//         manager.target(&second, 7.375).await.unwrap();
//         assert_eq!(manager.status(&second).await.unwrap().position, 7.375);
//         manager.sessions.lock().await.get_mut(&second).unwrap().touched = Some(Instant::now() - Duration::from_secs(61));
//         manager.expire().await.unwrap();
//         assert!(manager.status(&second).await.is_err());
//         tokio::time::timeout(Duration::from_secs(3), async {
//             while cache.get(&key).await.is_ok() { tokio::time::sleep(Duration::from_millis(5)).await; }
//         }).await.unwrap();
//         assert!(manager.media_path(&second, "init.mp4").await.is_err());
//         manager.shutdown().await.unwrap();
//         drop(permit);
//         drop(output);
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }
// }
