use std::{collections::HashMap, path::PathBuf, sync::Arc, time::{Duration, Instant}};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::{sync::Mutex, time::interval};
use tracing::warn;

use super::{cache::{random_id, CacheEntry, CacheManager, CacheState}, ffmpeg::{self, EncodeSpec}};

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub url: String,
    pub mime_type: &'static str,
    pub start_time: f64,
    pub duration: f64,
    pub state: CacheState,
    pub ready: bool,
    pub buffered_until: f64,
    pub error: Option<String>,
}

struct Session {
    cache: Arc<CacheEntry>,
    duration: f64,
    touched: Instant,
}

#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    cache: CacheManager,
    idle_seconds: u64,
}

impl SessionManager {
    pub fn new(cache: CacheManager, idle_seconds: u64) -> Self {
        Self { sessions: Arc::new(Mutex::new(HashMap::new())), cache, idle_seconds }
    }

    pub async fn create(&self, spec: EncodeSpec) -> Result<SessionStatus> {
        let mut sessions = self.sessions.lock().await;
        ensure!(!self.cache.stopping(), "server is shutting down");
        ensure!(sessions.len() < 32, "too many playback sessions; stop an existing stream");
        let id = random_id()?;
        let duration = spec.duration;
        let cache = self.cache.acquire(spec).await?;
        sessions.insert(id.clone(), Session { cache, duration, touched: Instant::now() });
        drop(sessions);
        self.status(&id).await
    }

    pub async fn status(&self, id: &str) -> Result<SessionStatus> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id).context("playback session not found or expired")?;
        session.touched = Instant::now();
        let status = session.cache.status().await;
        Ok(SessionStatus {
            session_id: id.to_string(), url: format!("/playback/{id}/index.m3u8"),
            mime_type: "application/vnd.apple.mpegurl", start_time: 0.0,
            duration: session.duration, state: status.state, ready: status.ready(),
            buffered_until: status.buffered_until.min(session.duration), error: status.error,
        })
    }

    pub async fn heartbeat(&self, id: &str) -> Result<()> {
        self.status(id).await?;
        Ok(())
    }

    pub async fn media_path(&self, id: &str, name: &str) -> Result<(PathBuf, &'static str, Arc<CacheEntry>)> {
        ensure!(ffmpeg::media_filename(name), "playback asset not found");
        let cache = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(id).context("playback session not found or expired")?;
            session.touched = Instant::now();
            session.cache.clone()
        };
        ensure!(cache.status().await.ready(), "playback output is not ready");
        let path = cache.directory.join(name);
        let file = tokio::fs::symlink_metadata(&path).await.context("cached media file is missing")?;
        ensure!(file.is_file() && file.len() > 0, "unsafe or empty cached media file");
        let mime = if name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" }
            else if name.ends_with(".m4s") { "video/iso.segment" } else { "video/mp4" };
        // Pin the entry until ServeFile has opened the file, even if DELETE races with this request.
        Ok((path, mime, cache))
    }

    pub async fn release(&self, id: &str) -> Result<()> {
        self.sessions.lock().await.remove(id);
        self.cache.maintain().await
    }

    pub fn spawn_expiration(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if manager.cache.stopping() { break; }
                if let Err(error) = manager.expire().await {
                    warn!("playback cache maintenance failed: {error:#}");
                }
            }
        });
    }

    async fn expire(&self) -> Result<()> {
        self.sessions.lock().await.retain(|_, session| session.touched.elapsed().as_secs() < self.idle_seconds);
        self.cache.maintain().await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.sessions.lock().await.clear();
        self.cache.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::PlaybackConfig, video::{ffmpeg::fixture, process::Processes}};

    #[tokio::test]
    async fn sessions_share_generation_and_expiry_only_releases_leases() {
        let root = std::env::temp_dir().join(format!("anime-leases-{}", random_id().unwrap()));
        let processes = Processes::new(1);
        let permit = processes.slots.acquire().await.unwrap();
        let cache = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
        let manager = SessionManager::new(cache, 60);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let (info, request) = fixture();
            let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, 1).unwrap();
            ids.push(manager.create(spec).await.unwrap().session_id);
        }
        assert_ne!(ids[0], ids[1]);
        {
            let sessions = manager.sessions.lock().await;
            assert!(Arc::ptr_eq(&sessions[&ids[0]].cache, &sessions[&ids[1]].cache));
        }
        manager.release(&ids[0]).await.unwrap();
        assert!(manager.status(&ids[0]).await.is_err());
        manager.sessions.lock().await.get_mut(&ids[1]).unwrap().touched = Instant::now() - Duration::from_secs(61);
        manager.expire().await.unwrap();
        assert!(manager.status(&ids[1]).await.is_err());
        assert!(tokio::fs::read_dir(root.join("playback")).await.unwrap().next_entry().await.unwrap().is_some());
        manager.shutdown().await.unwrap();
        drop(permit);
        assert!(tokio::fs::read_dir(root.join("playback")).await.unwrap().next_entry().await.unwrap().is_none());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
