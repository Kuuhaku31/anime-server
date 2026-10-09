use std::{
    collections::HashMap, io::Read, path::{Path, PathBuf}, sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt}, process::Child, sync::{watch, Mutex},
    task::JoinHandle, time::{interval, timeout},
};
use tracing::{info, warn};

use crate::config::PlaybackConfig;
use super::{assets, ffmpeg::{self, EncodeSpec}, process::Processes};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState { Queued, Starting, Ready, Complete, Failed, Cancelled }

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub url: String,
    pub mime_type: &'static str,
    pub start_time: f64,
    pub duration: f64,
    pub state: SessionState,
    pub ready: bool,
    pub buffered_until: f64,
    pub error: Option<String>,
}

struct Session {
    directory: PathBuf,
    status: Arc<Mutex<SessionStatus>>,
    cancel: watch::Sender<bool>,
    worker: JoinHandle<Result<()>>,
    touched: Instant,
}

#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    root: PathBuf,
    config: PlaybackConfig,
    processes: Processes,
}

impl SessionManager {
    pub async fn new(root: PathBuf, config: PlaybackConfig, processes: Processes) -> Result<Self> {
        tokio::fs::create_dir_all(&root).await?;
        clean_startup(&root).await?;
        let probe = root.join(session_id()?);
        tokio::fs::create_dir(&probe).await.context("playback session directory is not writable")?;
        tokio::fs::remove_dir(probe).await?;
        Ok(Self { sessions: Arc::new(Mutex::new(HashMap::new())), root, config, processes })
    }

    pub async fn create(&self, spec: EncodeSpec) -> Result<SessionStatus> {
        let mut sessions = self.sessions.lock().await;
        ensure!(!self.processes.slots.is_closed(), "server is shutting down");
        ensure!(sessions.len() < 32, "too many playback sessions; stop an existing stream");
        let id = session_id()?;
        let directory = self.root.join(&id);
        let status = SessionStatus {
            session_id: id.clone(), url: format!("/playback/{id}/index.m3u8"),
            mime_type: "application/vnd.apple.mpegurl",
            start_time: spec.start, duration: spec.duration,
            state: SessionState::Queued, ready: false, buffered_until: spec.start, error: None,
        };
        let shared = Arc::new(Mutex::new(status.clone()));
        let (cancel, receiver) = watch::channel(false);
        let worker = tokio::spawn(run_session(spec, directory.clone(), shared.clone(),
            self.processes.clone(), self.config.startup_timeout_seconds, receiver));
        sessions.insert(id, Session { directory, status: shared, cancel, worker, touched: Instant::now() });
        Ok(status)
    }

    pub async fn status(&self, id: &str) -> Result<SessionStatus> {
        let status = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(id).context("playback session not found or expired")?;
            session.touched = Instant::now();
            session.status.clone()
        };
        let value = status.lock().await.clone();
        Ok(value)
    }

    pub async fn heartbeat(&self, id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        sessions.get_mut(id).context("playback session not found or expired")?.touched = Instant::now();
        Ok(())
    }

    pub async fn media_path(&self, id: &str, name: &str) -> Result<(PathBuf, &'static str)> {
        ensure!(ffmpeg::media_filename(name), "playback asset not found");
        let (directory, status) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(id).context("playback session not found or expired")?;
            session.touched = Instant::now();
            (session.directory.clone(), session.status.clone())
        };
        ensure!(status.lock().await.ready, "playback output is not ready");
        let mime = if name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" }
            else if name.ends_with(".m4s") { "video/iso.segment" } else { "video/mp4" };
        Ok((directory.join(name), mime))
    }

    pub async fn release(&self, id: &str) -> Result<()> {
        let session = self.sessions.lock().await.remove(id);
        if let Some(session) = session {
            session.cancel.send_replace(true);
            session.worker.await.context("playback session task failed")??;
        }
        Ok(())
    }

    pub fn spawn_expiration(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if manager.processes.slots.is_closed() { break; }
                if let Err(error) = manager.expire().await {
                    warn!("playback session cleanup failed: {error:#}");
                }
            }
        });
    }

    async fn expire(&self) -> Result<()> {
        let expired = {
            let mut sessions = self.sessions.lock().await;
            let ids: Vec<_> = sessions.iter().filter(|(_, session)|
                session.touched.elapsed().as_secs() >= self.config.idle_timeout_seconds)
                .map(|(id, _)| id.clone()).collect();
            ids.into_iter().filter_map(|id| sessions.remove(&id)).collect::<Vec<_>>()
        };
        stop_sessions(expired).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.processes.slots.close();
        let sessions = self.sessions.lock().await.drain().map(|(_, session)| session).collect();
        stop_sessions(sessions).await
    }
}

async fn stop_sessions(sessions: Vec<Session>) -> Result<()> {
    for session in &sessions { session.cancel.send_replace(true); }
    let mut errors = Vec::new();
    for session in sessions {
        match session.worker.await {
            Ok(Ok(())) => {},
            Ok(Err(error)) => errors.push(format!("{error:#}")),
            Err(error) => errors.push(error.to_string()),
        }
    }
    ensure!(errors.is_empty(), "session cleanup failed: {}", errors.join("; "));
    Ok(())
}

async fn run_session(spec: EncodeSpec, directory: PathBuf, status: Arc<Mutex<SessionStatus>>,
    processes: Processes, startup_seconds: u64, mut cancel: watch::Receiver<bool>) -> Result<()> {
    let result = encode(&spec, &directory, &status, &processes, startup_seconds, &mut cancel).await;
    if !*cancel.borrow() {
        match result {
            Ok(()) => cancelled(&mut cancel).await,
            Err(error) => {
                let mut status = status.lock().await;
                status.state = SessionState::Failed;
                status.ready = false;
                status.error = Some(format!("{error:#}"));
                warn!(session_id = status.session_id, "playback failed: {error:#}");
            }
        }
    }
    if *cancel.borrow() {
        let mut status = status.lock().await;
        status.state = SessionState::Cancelled;
        status.ready = false;
    }
    match tokio::fs::remove_dir_all(&directory).await {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error).context("cannot remove playback session directory"),
    }
    Ok(())
}

async fn encode(spec: &EncodeSpec, directory: &Path, status: &Mutex<SessionStatus>,
    processes: &Processes, startup_seconds: u64, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let _permit = tokio::select! {
        biased;
        _ = cancelled(cancel) => bail!("playback cancelled"),
        permit = processes.slots.acquire() => permit?,
    };
    tokio::fs::create_dir(directory).await?;
    status.lock().await.state = SessionState::Starting;
    tokio::select! {
        biased;
        _ = cancelled(cancel) => bail!("playback cancelled"),
        result = timeout(Duration::from_secs(startup_seconds), assets::prepare(spec, directory)) => {
            result.context("subtitle preparation timed out")??;
        },
    }
    let mut child = ffmpeg::command(spec, directory).spawn().context("failed to start FFmpeg")?;
    let stderr = child.stderr.take().context("FFmpeg stderr is unavailable")?;
    let reader = tokio::spawn(read_tail(stderr));
    let id = status.lock().await.session_id.clone();
    info!(session_id = id, pid = child.id(), "playback started");
    let result = tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(anyhow::anyhow!("playback cancelled")),
        result = monitor(spec, directory, status, &mut child, startup_seconds) => result,
    };
    if child.try_wait()?.is_none() { child.kill().await?; }
    child.wait().await?;
    let tail = reader.await.context("FFmpeg error reader failed")??;
    result.with_context(|| if tail.is_empty() { "FFmpeg session ended".to_string() } else { format!("FFmpeg: {tail}") })
}

async fn monitor(spec: &EncodeSpec, directory: &Path, status: &Mutex<SessionStatus>,
    child: &mut Child, startup_seconds: u64) -> Result<()> {
    let started = Instant::now();
    let mut ready = false;
    let mut tick = interval(Duration::from_millis(400));
    loop {
        tick.tick().await;
        let exit = child.try_wait()?;
        if let Some(exit) = exit { ensure!(exit.success(), "FFmpeg exited with {exit}"); }
        if let Some(duration) = ffmpeg::playlist_duration(directory).await? {
            if !ready {
                ffmpeg::validate_first_segment(directory, spec.audio_index.is_some()).await?;
                ready = true;
            }
            let mut status = status.lock().await;
            status.ready = true;
            status.buffered_until = (spec.start + duration).min(spec.duration);
            status.state = if exit.is_some() { SessionState::Complete } else { SessionState::Ready };
            if exit.is_some() { return Ok(()); }
        } else {
            ensure!(exit.is_none(), "FFmpeg completed without a playable segment");
        }
        ensure!(ready || started.elapsed().as_secs() < startup_seconds, "FFmpeg startup timed out");
    }
}

async fn read_tail(mut reader: impl AsyncRead + Unpin) -> Result<String> {
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 { break; }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 4000 { tail.drain(..tail.len() - 4000); }
    }
    Ok(String::from_utf8_lossy(&tail).into_owned())
}

async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    if !*receiver.borrow() { let _ = receiver.changed().await; }
}

pub fn session_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn clean_startup(root: &Path) -> Result<()> {
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) && entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ffmpeg::fixture;

    #[tokio::test]
    async fn queued_sessions_are_distinct_cancellable_and_expire() {
        let root = std::env::temp_dir().join(format!("anime-session-test-{}", session_id().unwrap()));
        let processes = Processes::new(1);
        let permit = processes.slots.acquire().await.unwrap();
        let manager = SessionManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let (info, request) = fixture();
            let spec = EncodeSpec::new("video.mkv".into(), info, request, 1).unwrap();
            ids.push(manager.create(spec).await.unwrap().session_id);
        }
        assert_ne!(ids[0], ids[1]);
        assert_eq!(manager.status(&ids[0]).await.unwrap().state, SessionState::Queued);
        manager.release(&ids[0]).await.unwrap();
        assert!(manager.status(&ids[0]).await.is_err());
        manager.sessions.lock().await.get_mut(&ids[1]).unwrap().touched = Instant::now() - Duration::from_secs(61);
        manager.expire().await.unwrap();
        assert!(manager.status(&ids[1]).await.is_err());
        assert!(tokio::fs::read_dir(&root).await.unwrap().next_entry().await.unwrap().is_none());
        drop(permit);
        manager.shutdown().await.unwrap();
        tokio::fs::remove_dir(root).await.unwrap();
    }

    #[tokio::test]
    async fn startup_only_removes_known_session_directories() {
        let root = std::env::temp_dir().join(format!("anime-startup-test-{}", session_id().unwrap()));
        tokio::fs::create_dir(&root).await.unwrap();
        let abandoned = root.join(session_id().unwrap());
        tokio::fs::create_dir(&abandoned).await.unwrap();
        tokio::fs::write(abandoned.join("init.mp4"), b"unfinished").await.unwrap();
        let unrelated = root.join("keep.txt");
        tokio::fs::write(&unrelated, b"keep").await.unwrap();
        let manager = SessionManager::new(root.clone(), PlaybackConfig::default(), Processes::new(1)).await.unwrap();
        assert!(!abandoned.exists());
        assert!(unrelated.exists());
        manager.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn stderr_retains_only_a_bounded_tail() {
        let mut input = vec![b'a'; 6000];
        input.extend_from_slice(b"last error");
        let tail = read_tail(input.as_slice()).await.unwrap();
        assert_eq!(tail.len(), 4000);
        assert!(tail.ends_with("last error"));
    }
}
