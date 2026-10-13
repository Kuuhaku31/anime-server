//! 管理不透明播放会话并协调活动缓存租约.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::{Duration, Instant}};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::{sync::Mutex, time};
use tracing::warn;

use crate::models::{PlaybackRequest, PlaybackStart, PlaybackVariantConfig};
use crate::cache::{CacheManager, CachePhase};

#[cfg(test)]
#[path = "tests/sessions.rs"]
mod tests;


/// 对外只暴露不透明会话和状态快照, 不返回播放配置或 variant-key.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionStatus {
    session_id: String,
    url: String,
    mime_type: &'static str,
    start_time: f64,
    pub(crate) duration: f64,
    position: f64,
    phase: CachePhase,
    buffered_until: f64,
}

// 会话保存变体键, 目标位置和存活时间, 媒体状态由缓存文件夹维护.
struct Session {
    variant_key: String,
    target: f64,
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
    pub(crate) async fn create(&self, cache_manager: &CacheManager, file_id: &str, request: PlaybackRequest) -> Result<PlaybackStart> {

        let media = cache_manager.media_info(file_id).await.map_err(|e| anyhow::anyhow!(e))?;
        let config = PlaybackVariantConfig::new(file_id, &media, &request)?;

        // 合并显式位置和历史位置, 保持已看完的视频从头开始播放.
        let saved = request.saved_position().unwrap_or(0.0);
        ensure!(saved.is_finite() && saved >= 0.0, "invalid saved playback position");
        let target = request.position.unwrap_or(if saved < media.duration - 2.0 { saved } else { 0.0 });
        ensure!(target.is_finite() && target >= 0.0 && target < media.duration, "position must be within the video duration");

        let session_id = crate::utils::random_id()?;
        let variant_key = config.variant_key().to_owned();
        let selection = config.selection();
        {
            let mut sessions = self.sessions.lock().await;
            ensure!(!cache_manager.is_stopping(), "server is shutting down");
            ensure!(sessions.len() < 32, "too many playback sessions; stop an existing stream");
            sessions.insert(session_id.clone(), Session {
                variant_key: variant_key.clone(), target, created: Instant::now(), touched: None,
            });
        }
        // 缓存恢复时不持有会话表锁, 停止或并发激活冲突直接交由创建失败流程清理.
        let result = async {
            let cache = cache_manager.acquire(&config).await?;
            let threads = self.desired_threads(cache_manager, &variant_key, cache.buffered_until()).await?;
            cache_manager.start(&variant_key, threads).await?;
            self.status(cache_manager, &session_id).await
        }.await;
        match result {
            Ok(session) => Ok(PlaybackStart::new(session, media, selection.0, selection.1)),
            Err(error) => {
                self.sessions.lock().await.remove(&session_id);
                if let Err(cleanup) = self.refresh_variant(cache_manager, &variant_key).await {
                    return Err(error.context(format!("session cleanup failed: {cleanup:#}")));
                }
                Err(error)
            },
        }
    }

    pub(crate) async fn status(&self, cache_manager: &CacheManager, session_id: &str) -> Result<SessionStatus> {
        let (variant_key, target_position) = self.get_session(session_id).await?;
        let cache_status = cache_manager.status(&variant_key).await?;
        Ok(SessionStatus {
            session_id: session_id.to_string(),
            url: format!("/playback/{session_id}/index.m3u8?revision={}", cache_status.revision()),
            mime_type: "application/vnd.apple.mpegurl", start_time: 0.0, duration: cache_status.duration(),
            position: target_position, phase: cache_status.phase(), buffered_until: cache_status.buffered_until(),
        })
    }

    pub(crate) async fn heartbeat(&self, cache_manager: &CacheManager, session_id: &str) -> Result<()> {
        self.status(cache_manager, session_id).await?;
        Ok(())
    }

    pub(crate) async fn target(
        &self, cache_manager: &CacheManager, session_id: &str, target_position: f64,
    ) -> Result<SessionStatus> {
        let current = self.status(cache_manager, session_id).await?;
        ensure!(target_position.is_finite() && target_position >= 0.0 && target_position < current.duration,
            "position must be within the video duration");
        {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(session_id).context("playback session not found or expired")?;
            session.target = target_position;
            session.touched = Some(Instant::now());
        }
        self.status(cache_manager, session_id).await
    }

    #[allow(dead_code)]
    pub(crate) async fn pause(&self, cache_manager: &CacheManager, session_id: &str) -> Result<()> {
        let (variant_key, _) = self.get_session(session_id).await?;
        cache_manager.pause(&variant_key).await
    }

    /// 续期会话并返回已验证的媒体路径, HTTP 媒体类型由 API 处理.
    pub(crate) async fn media_path(&self, cache_manager: &CacheManager, session_id: &str, asset_name: &str)
        -> Result<PathBuf>
    {
        let (variant_key, _) = self.get_session(session_id).await?;
        cache_manager.media_path(&variant_key, asset_name).await
    }

    /// 先移除会话, 仅最后一个会话离开时停用共享缓存.
    pub(crate) async fn release(&self, cache_manager: &CacheManager, session_id: &str) -> Result<()> {
        let session = self.sessions.lock().await.remove(session_id);
        if let Some(session) = session { self.refresh_variant(cache_manager, &session.variant_key).await?; }
        Ok(())
    }

    pub(crate) fn spawn_expiration(&self, cache_manager: CacheManager) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut tick = time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if cache_manager.is_stopping() { break; }
                if let Err(error) = expire(&manager, &cache_manager).await {
                    warn!("playback cache maintenance failed: {error:#}");
                }
            }
        });
    }

    pub(crate) async fn shutdown(&self) {
        self.sessions.lock().await.clear();
    }

    // 状态, 媒体和显式心跳请求都经此续期, 客户端不能自行提供缓存键.
    async fn get_session(&self, session_id: &str) -> Result<(String, f64)> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(session_id).context("playback session not found or expired")?;
        session.touched = Some(Instant::now());
        Ok((session.variant_key.clone(), session.target))
    }

    async fn desired_threads(
        &self, cache_manager: &CacheManager, variant_key: &str, buffered_until: f64,
    ) -> Result<usize> {
        let sessions = self.sessions.lock().await;
        let target = sessions.values()
            .filter(|session| session.variant_key == variant_key)
            .map(|session| session.target)
            .max_by(f64::total_cmp);
        let target = target.context("playback session ended before cache generation started")?;
        let (base, maximum) = cache_manager.thread_limits();
        Ok(select_threads(target, buffered_until, base, maximum))
    }

    async fn refresh_variant(&self, cache_manager: &CacheManager, variant_key: &str) -> Result<()> {
        let sessions = self.sessions.lock().await;
        if !sessions.values().any(|session| session.variant_key == variant_key) {
            cache_manager.release(variant_key).await?;
        }
        Ok(())
    }
}

// 只由 SessionManager 根据位置快照选择线程数, CacheManager 不读取 position.
fn select_threads(target: f64, buffered_until: f64, base: usize, maximum: usize) -> usize {
    if target >= buffered_until { maximum } else { base }
}

// 锁内批量摘除过期会话, 锁外逐一清理, 单个失败不能阻断其余租约释放.
async fn expire(manager: &SessionManager, cache_manager: &CacheManager) -> Result<()> {
    let expired = {
        let mut sessions = manager.sessions.lock().await;
        let expired_session_ids: Vec<_> = sessions.iter().filter(|(_, session)| match session.touched {
            Some(touched) => touched.elapsed().as_secs() >= manager.idle_seconds,
            None => session.created.elapsed().as_secs() >= manager.idle_seconds.max(120),
        })
            .map(|(session_id, _)| session_id.clone()).collect();
        expired_session_ids.into_iter()
            .filter_map(|session_id| sessions.remove(&session_id)).collect::<Vec<_>>()
    };
    let mut errors = Vec::new();
    let variants: std::collections::HashSet<_> = expired.iter().map(|session| session.variant_key.clone()).collect();
    for variant_key in variants {
        if let Err(error) = manager.refresh_variant(cache_manager, &variant_key).await {
            errors.push(format!("variant {variant_key}: {error:#}"));
        }
    }
    if let Err(error) = cache_manager.maintain().await { errors.push(format!("{error:#}")); }
    ensure!(errors.is_empty(), "session expiration failed: {}", errors.join("; "));
    Ok(())
}
