use std::{
    collections::HashMap, fs::{File, FileTimes}, io::Read, path::{Path, PathBuf},
    sync::Arc, time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use md5::{Digest, Md5};
use serde::Serialize;
use tokio::{process::Child, sync::{watch, Mutex}, task::JoinHandle, time::interval};
use tracing::{info, warn};

use crate::config::PlaybackConfig;
use super::{assets::{self, RenderAssets}, ffmpeg::{self, EncodeSpec},
    process::{cancelled, read_tail, Processes}};

pub const ENCODER_PROFILE: &str = "h264-high-yuv420p-aac-fmp4-v1";
const MAX_GENERATIONS: usize = 32;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PlaybackVariantConfig {
    pub src_file_pre16mib_md5: String,
    pub video_stream: i32,
    pub audio_stream: Option<i32>,
    pub subtitle_stream: Option<i32>,
    pub encoder_profile: String,
}

impl PlaybackVariantConfig {
    pub fn key(&self) -> Result<String> {
        Ok(format!("{:x}", Md5::digest(serde_json::to_vec(self)?)))
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheState { Queued, Starting, Ready, Complete, Failed, Cancelled }

#[derive(Clone)]
pub struct CacheStatus {
    pub state: CacheState,
    pub buffered_until: f64,
    pub error: Option<String>,
    touched: SystemTime,
    flushed: SystemTime,
}

impl CacheStatus {
    pub fn ready(&self) -> bool {
        matches!(self.state, CacheState::Ready | CacheState::Complete)
    }
}

pub struct CacheEntry {
    pub key: String,
    pub directory: PathBuf,
    config: Option<PlaybackVariantConfig>,
    status: Mutex<CacheStatus>,
}

impl CacheEntry {
    pub async fn status(&self) -> CacheStatus {
        let mut status = self.status.lock().await;
        status.touched = SystemTime::now();
        status.clone()
    }
}

struct Entry {
    output: Arc<CacheEntry>,
    worker: Option<JoinHandle<()>>,
}

struct CacheInner {
    root: PathBuf,
    config: PlaybackConfig,
    processes: Processes,
    entries: Mutex<HashMap<String, Entry>>,
    stop: watch::Sender<bool>,
    _directory_lock: File,
}

#[derive(Clone)]
pub struct CacheManager {
    inner: Arc<CacheInner>,
}

impl CacheManager {
    pub async fn new(root: PathBuf, config: PlaybackConfig, processes: Processes) -> Result<Self> {
        config.validate()?;
        tokio::fs::create_dir_all(&root).await?;
        let root = tokio::fs::canonicalize(root).await?;
        let directory_lock = File::open(&root)?;
        directory_lock.try_lock().context("cache directory is already in use by another server")?;
        for name in ["playback", "subtitles", "fonts"] {
            let directory = root.join(name);
            tokio::fs::create_dir_all(&directory).await?;
            ensure!(tokio::fs::symlink_metadata(&directory).await?.is_dir(), "unsafe cache directory: {name}");
            let probe = directory.join(format!(".write-{}", random_id()?));
            tokio::fs::OpenOptions::new().write(true).create_new(true).open(&probe).await
                .with_context(|| format!("cache directory is not writable: {}", directory.display()))?;
            tokio::fs::remove_file(probe).await?;
        }
        assets::clean_startup(&root).await?;
        let (stop, _) = watch::channel(false);
        let manager = Self { inner: Arc::new(CacheInner {
            root, config, processes, entries: Mutex::new(HashMap::new()), stop,
            _directory_lock: directory_lock,
        }) };
        manager.load().await?;
        manager.maintain().await?;
        Ok(manager)
    }

    pub fn stopping(&self) -> bool { *self.inner.stop.borrow() }

    async fn load(&self) -> Result<()> {
        let mut directories = tokio::fs::read_dir(self.inner.root.join("playback")).await?;
        let mut entries = self.inner.entries.lock().await;
        while let Some(directory) = directories.next_entry().await? {
            let key = directory.file_name().to_string_lossy().into_owned();
            if !is_key(&key) { continue; }
            let path = directory.path();
            ensure!(directory.file_type().await?.is_dir(), "unsafe cache entry: {key}");
            let touched = directory.metadata().await?.modified()?;
            let result = async {
                let marker = tokio::fs::symlink_metadata(path.join("complete")).await?;
                ensure!(marker.is_file() && marker.len() == 0, "invalid cache completion marker");
                let duration = ffmpeg::validate_final_playlist(&path).await?;
                ffmpeg::validate_first_segment(&path, None, &mut self.inner.stop.subscribe()).await?;
                Ok::<_, anyhow::Error>(duration)
            }.await;
            match result {
                Ok(duration) => {
                    entries.insert(key.clone(), Entry {
                        output: Arc::new(CacheEntry {
                            key, directory: path, config: None,
                            status: Mutex::new(CacheStatus {
                                state: CacheState::Complete, buffered_until: duration, error: None,
                                touched, flushed: touched,
                            }),
                        }),
                        worker: None,
                    });
                },
                Err(error) => {
                    warn!(variant_key = key, "discarding incomplete or invalid cache: {error:#}");
                    tokio::fs::remove_dir_all(path).await?;
                },
            }
        }
        Ok(())
    }

    pub async fn acquire(&self, spec: EncodeSpec) -> Result<Arc<CacheEntry>> {
        let key = spec.config.key()?;
        let mut entries = self.inner.entries.lock().await;
        ensure!(!self.stopping(), "server is shutting down");
        if let Some(entry) = entries.get(&key) {
            let status = entry.output.status.lock().await.clone();
            if matches!(status.state, CacheState::Failed | CacheState::Cancelled) {
                ensure!(Arc::strong_count(&entry.output) == 1,
                    "cached generation failed; release its remaining sessions before retrying: {}",
                    status.error.as_deref().unwrap_or("generation cancelled"));
                remove_output(&entry.output.directory).await?;
                entries.remove(&key);
            } else {
                ensure!(entry.output.config.as_ref().is_none_or(|config| config == &spec.config),
                    "playback configuration hash collision");
                if status.state == CacheState::Complete {
                    if let Err(error) = ffmpeg::validate_final_playlist(&entry.output.directory).await {
                        let mut status = entry.output.status.lock().await;
                        status.state = CacheState::Failed;
                        status.error = Some(format!("cached HLS is invalid: {error:#}"));
                        bail!("cached HLS is invalid: {error:#}");
                    }
                }
                entry.output.status().await;
                return Ok(entry.output.clone());
            }
        }
        let mut generating = 0;
        for entry in entries.values() {
            if matches!(entry.output.status.lock().await.state, CacheState::Queued | CacheState::Starting | CacheState::Ready) {
                generating += 1;
            }
        }
        ensure!(generating < MAX_GENERATIONS, "too many cache generation tasks; wait for an existing task to finish");
        let directory = self.inner.root.join("playback").join(&key);
        tokio::fs::create_dir(&directory).await.context("cannot create playback cache")?;
        let touched = SystemTime::now();
        let output = Arc::new(CacheEntry {
            key: key.clone(), directory, config: Some(spec.config.clone()),
            status: Mutex::new(CacheStatus {
                state: CacheState::Queued, buffered_until: 0.0, error: None, touched, flushed: touched,
            }),
        });
        let manager = self.clone();
        let work = output.clone();
        let worker = tokio::spawn(async move {
            manager.generate(spec, &work).await;
            drop(work);
            if let Err(error) = manager.maintain().await {
                warn!("cache cleanup after generation failed: {error:#}");
            }
        });
        entries.insert(key, Entry { output: output.clone(), worker: Some(worker) });
        Ok(output)
    }

    async fn generate(&self, spec: EncodeSpec, output: &CacheEntry) {
        let result = self.encode(&spec, output).await;
        match result {
            Ok(duration) => {
                let mut status = output.status.lock().await;
                status.buffered_until = duration;
                status.state = CacheState::Complete;
                info!(variant_key = output.key, "playback cache complete");
            },
            Err(error) => {
                {
                    let mut status = output.status.lock().await;
                    status.state = if self.stopping() { CacheState::Cancelled } else { CacheState::Failed };
                    status.error = Some(format!("{error:#}"));
                }
                warn!(variant_key = output.key, "cache generation failed: {error:#}");
                if let Err(cleanup) = remove_output(&output.directory).await {
                    warn!(variant_key = output.key, "cache output cleanup failed: {cleanup:#}");
                    output.status.lock().await.error = Some(format!("{error:#}; cleanup failed: {cleanup:#}"));
                }
            },
        }
    }

    async fn encode(&self, spec: &EncodeSpec, output: &CacheEntry) -> Result<f64> {
        let mut cancel = self.inner.stop.subscribe();
        let _permit = tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => bail!("cache generation cancelled"),
            permit = self.inner.processes.slots.acquire() => permit?,
        };
        output.status.lock().await.state = CacheState::Starting;
        let assets = if spec.subtitle.as_ref().is_some_and(|track| !track.is_bitmap()) {
            Some(RenderAssets::allocate(&self.inner.root).await?)
        } else { None };
        let result = self.render(spec, output, assets.as_ref(), &mut cancel).await;
        let cleanup = match assets {
            Some(assets) => assets.cleanup().await,
            None => Ok(()),
        };
        let duration = match (result, cleanup) {
            (Ok(duration), Ok(())) => duration,
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(error), Err(cleanup)) => bail!("{error:#}; {cleanup:#}"),
        };
        let marker = tokio::fs::OpenOptions::new().write(true).create_new(true)
            .open(output.directory.join("complete")).await?;
        marker.sync_all().await?;
        let mut status = output.status.lock().await;
        persist_used(&output.directory, status.touched).await?;
        status.flushed = status.touched;
        Ok(duration)
    }

    async fn render(&self, spec: &EncodeSpec, output: &CacheEntry, assets: Option<&RenderAssets>,
        cancel: &mut watch::Receiver<bool>) -> Result<f64> {
        let seconds = self.inner.config.startup_timeout_seconds;
        if let Some(assets) = assets { assets.prepare(spec, seconds, cancel).await?; }
        let mut child = ffmpeg::command(spec, &output.directory, assets)?.spawn().context("failed to start FFmpeg")?;
        let stderr = child.stderr.take().context("FFmpeg stderr is unavailable")?;
        let reader = tokio::spawn(read_tail(stderr));
        info!(variant_key = output.key, pid = child.id(), "cache generation started");
        let result = monitor(spec, output, &mut child, seconds, cancel).await;
        if child.try_wait()?.is_none() { child.kill().await?; }
        child.wait().await?;
        let tail = reader.await.context("FFmpeg error reader failed")??;
        result.with_context(|| if tail.is_empty() { "FFmpeg generation ended".to_string() } else { format!("FFmpeg: {tail}") })?;
        ffmpeg::validate_final_playlist(&output.directory).await
    }

    pub async fn maintain(&self) -> Result<()> {
        let mut entries = self.inner.entries.lock().await;
        let mut candidates = Vec::new();
        let mut failed = Vec::new();
        for (key, entry) in entries.iter() {
            let mut status = entry.output.status.lock().await;
            if status.state == CacheState::Complete {
                if status.flushed != status.touched {
                    persist_used(&entry.output.directory, status.touched).await?;
                    status.flushed = status.touched;
                }
                // The table owns one reference; sessions, generation and media reads pin additional references.
                if Arc::strong_count(&entry.output) == 1 {
                    candidates.push((status.touched, key.clone()));
                }
            } else if matches!(status.state, CacheState::Failed | CacheState::Cancelled)
                && Arc::strong_count(&entry.output) == 1 {
                failed.push(key.clone());
            }
        }
        for key in failed {
            remove_output(&self.inner.root.join("playback").join(&key)).await?;
            entries.remove(&key);
        }
        let root = self.inner.root.clone();
        let (mut total, sizes) = tokio::task::spawn_blocking(move || disk_usage(&root)).await
            .context("cache size scan failed")??;
        candidates.sort();
        for (_, key) in candidates {
            if total <= self.inner.config.cache_max_bytes { break; }
            remove_output(&self.inner.root.join("playback").join(&key)).await?;
            entries.remove(&key);
            total = total.saturating_sub(*sizes.get(&key).context("cache directory disappeared during size scan")?);
            info!(variant_key = key, "evicted least recently used playback cache");
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.inner.stop.send_replace(true);
        self.inner.processes.slots.close();
        let workers: Vec<_> = self.inner.entries.lock().await.values_mut()
            .filter_map(|entry| entry.worker.take()).collect();
        let mut errors = Vec::new();
        for worker in workers {
            if let Err(error) = worker.await { errors.push(error.to_string()); }
        }
        if let Err(error) = self.maintain().await { errors.push(format!("{error:#}")); }
        ensure!(errors.is_empty(), "cache shutdown failed: {}", errors.join("; "));
        Ok(())
    }
}

async fn monitor(spec: &EncodeSpec, output: &CacheEntry, child: &mut Child, seconds: u64,
    cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let started = Instant::now();
    let mut ready = false;
    let mut tick = interval(Duration::from_millis(400));
    loop {
        tokio::select! {
            biased;
            _ = cancelled(cancel) => bail!("cache generation cancelled"),
            _ = tick.tick() => {},
        }
        let exit = child.try_wait()?;
        if let Some(exit) = exit { ensure!(exit.success(), "FFmpeg exited with {exit}"); }
        if let Some(playlist) = ffmpeg::read_playlist(&output.directory).await? {
            if !playlist.segments.is_empty() {
                if !ready {
                    ffmpeg::validate_first_segment(&output.directory, Some(spec.config.audio_stream.is_some()), cancel).await?;
                    ready = true;
                }
                let mut status = output.status.lock().await;
                status.buffered_until = playlist.duration;
                status.state = CacheState::Ready;
            }
        }
        if exit.is_some() {
            ensure!(ready, "FFmpeg completed without a playable segment");
            return Ok(());
        }
        ensure!(ready || started.elapsed().as_secs() < seconds, "FFmpeg startup timed out");
    }
}

async fn persist_used(directory: &Path, time: SystemTime) -> Result<()> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = File::open(directory)?;
        file.set_times(FileTimes::new().set_modified(time))?;
        file.sync_all()
    }).await.context("cache timestamp update task failed")??;
    Ok(())
}

async fn remove_output(directory: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(directory).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("cannot remove cache output"),
    }
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size = 0_u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.io_error().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => continue,
            Err(error) => return Err(error.into()),
        };
        if entry.file_type().is_file() {
            match entry.metadata() {
                Ok(metadata) => size = size.checked_add(metadata.len()).context("cache size overflow")?,
                Err(error) if error.io_error().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {},
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(size)
}

fn disk_usage(root: &Path) -> Result<(u64, HashMap<String, u64>)> {
    let mut sizes = HashMap::new();
    let mut total = directory_size(&root.join("subtitles"))?
        .checked_add(directory_size(&root.join("fonts"))?).context("cache size overflow")?;
    for entry in std::fs::read_dir(root.join("playback"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_key(&name) && entry.file_type()?.is_dir() {
            let size = directory_size(&entry.path())?;
            total = total.checked_add(size).context("cache size overflow")?;
            sizes.insert(name, size);
        }
    }
    Ok((total, sizes))
}

pub fn is_key(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PlaybackVariantConfig {
        PlaybackVariantConfig {
            src_file_pre16mib_md5: "35af6d912b6dffadc112df2ca94bbe2f".to_string(),
            video_stream: 0, audio_stream: Some(1), subtitle_stream: Some(3),
            encoder_profile: ENCODER_PROFILE.to_string(),
        }
    }

    #[test]
    fn playback_configuration_has_a_stable_canonical_key() {
        let original = config();
        assert_eq!(original.key().unwrap(), "bc892e2d4aa401399639fe22ee897b43");
        assert_eq!(original.key().unwrap(), original.clone().key().unwrap());
        for field in 0..6 {
            let mut changed = original.clone();
            match field {
                0 => changed.src_file_pre16mib_md5 = "a".repeat(32),
                1 => changed.video_stream = 2,
                2 => changed.audio_stream = None,
                3 => changed.subtitle_stream = Some(4),
                4 => changed.subtitle_stream = None,
                _ => changed.encoder_profile.push_str("-next"),
            }
            assert_ne!(changed.key().unwrap(), original.key().unwrap());
        }
        let mut silent = original;
        silent.audio_stream = None;
        silent.subtitle_stream = None;
        let json = String::from_utf8(serde_json::to_vec(&silent).unwrap()).unwrap();
        assert!(json.contains("\"audio_stream\":null,\"subtitle_stream\":null"));
        for key in ["../main.db", "ABCDEF0123456789ABCDEF0123456789", "a", ""] {
            assert!(!is_key(key));
        }
    }

    async fn fake_entry(manager: &CacheManager, name: char, size: usize, touched: u64,
        state: CacheState) -> Arc<CacheEntry> {
        let key = name.to_string().repeat(32);
        let directory = manager.inner.root.join("playback").join(&key);
        tokio::fs::create_dir(&directory).await.unwrap();
        tokio::fs::write(directory.join("init.mp4"), vec![0; size]).await.unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(touched);
        persist_used(&directory, time).await.unwrap();
        let output = Arc::new(CacheEntry {
            key: key.clone(), directory, config: None,
            status: Mutex::new(CacheStatus { state, buffered_until: 4.0, error: None, touched: time, flushed: time }),
        });
        manager.inner.entries.lock().await.insert(key, Entry { output: output.clone(), worker: None });
        output
    }

    #[tokio::test]
    async fn lru_protects_leases_and_generating_entries_and_cleans_after_release() {
        let root = std::env::temp_dir().join(format!("anime-lru-{}", random_id().unwrap()));
        let manager = CacheManager::new(root.clone(), PlaybackConfig {
            cache_max_bytes: 130, ..PlaybackConfig::default()
        }, Processes::new(1)).await.unwrap();
        let old = fake_entry(&manager, 'a', 60, 10, CacheState::Complete).await;
        let newer = fake_entry(&manager, 'b', 60, 20, CacheState::Complete).await;
        let active = fake_entry(&manager, 'c', 60, 5, CacheState::Complete).await;
        let old_path = old.directory.clone();
        let newer_path = newer.directory.clone();
        drop(old);
        drop(newer);
        manager.maintain().await.unwrap();
        assert!(!old_path.exists());
        assert!(newer_path.exists());
        assert!(active.directory.exists());
        let generating = fake_entry(&manager, 'd', 160, 1, CacheState::Ready).await;
        manager.maintain().await.unwrap();
        assert!(!newer_path.exists());
        assert_eq!(disk_usage(&root).unwrap().0, 220);
        let active_path = active.directory.clone();
        drop(active);
        manager.maintain().await.unwrap();
        assert!(!active_path.exists());
        assert!(generating.directory.exists());
        let generating_path = generating.directory.clone();
        generating.status.lock().await.state = CacheState::Complete;
        drop(generating);
        manager.maintain().await.unwrap();
        assert!(!generating_path.exists());
        assert_eq!(disk_usage(&root).unwrap().0, 0);
        manager.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn last_use_and_temporary_resources_are_accounted_for() {
        let root = std::env::temp_dir().join(format!("anime-usage-{}", random_id().unwrap()));
        let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(1)).await.unwrap();
        let entry = fake_entry(&manager, 'a', 60, 1, CacheState::Complete).await;
        let before = std::fs::metadata(&entry.directory).unwrap().modified().unwrap();
        let used = entry.status().await.touched;
        let assets = RenderAssets::allocate(&root).await.unwrap();
        tokio::fs::write(root.join("subtitles").join("test.ass"), b"1234567890").await.unwrap();
        assert_eq!(disk_usage(&root).unwrap().0, 70);
        manager.maintain().await.unwrap();
        assert!(used > before);
        assert_eq!(std::fs::metadata(&entry.directory).unwrap().modified().unwrap(), used);
        assets.cleanup().await.unwrap();
        drop(entry);
        manager.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn startup_removes_only_owned_incomplete_resources_and_locks_the_root() {
        let root = std::env::temp_dir().join(format!("anime-recovery-{}", random_id().unwrap()));
        let abandoned = root.join("playback").join("a".repeat(32));
        tokio::fs::create_dir_all(&abandoned).await.unwrap();
        tokio::fs::write(abandoned.join("init.mp4"), b"unfinished").await.unwrap();
        tokio::fs::create_dir(root.join("subtitles")).await.unwrap();
        tokio::fs::create_dir(root.join("fonts")).await.unwrap();
        let assets = RenderAssets::allocate(&root).await.unwrap();
        let foreign = root.join("subtitles").join("keep.txt");
        tokio::fs::write(&foreign, b"keep").await.unwrap();
        let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(1)).await.unwrap();
        assert!(!abandoned.exists());
        assert!(foreign.exists());
        assert!(tokio::fs::read_dir(root.join("fonts")).await.unwrap().next_entry().await.unwrap().is_none());
        assert!(CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(1)).await.is_err());
        assets.cleanup().await.unwrap();
        manager.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
