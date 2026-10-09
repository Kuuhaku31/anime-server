use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc, time::UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Mutex};
use tracing::warn;

use super::process::Processes;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub stream_index: i32,
    pub codec_name: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    #[serde(skip_serializing)]
    pub color_transfer: Option<String>,
    #[serde(skip_serializing)]
    pub external_path: Option<PathBuf>,
}

impl Track {
    pub fn is_bitmap(&self) -> bool {
        matches!(self.codec_name.as_str(), "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Attachment {
    pub stream_index: i32,
    pub ordinal: usize,
    pub font_extension: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub source_version: String,
    pub duration: f64,
    #[serde(skip_serializing)]
    pub start_time: f64,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub subtitle_tracks: Vec<Track>,
    #[serde(skip_serializing)]
    pub attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    start_time: Option<String>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: i32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    color_transfer: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    disposition: HashMap<String, i32>,
}

struct Cached {
    version: String,
    result: Result<MediaInfo, String>,
}

#[derive(Clone)]
pub struct MetadataCache {
    entries: Arc<Mutex<HashMap<String, Cached>>>,
    media_root: PathBuf,
    processes: Processes,
}

impl MetadataCache {
    pub fn new(media_root: PathBuf, processes: Processes) -> Self {
        Self { entries: Arc::new(Mutex::new(HashMap::new())), media_root, processes }
    }

    pub async fn get(&self, id: &str, path: &Path, retry: bool) -> Result<MediaInfo> {
        let sidecars = sidecars(path, &self.media_root).await?;
        let version = source_version(path, &sidecars).await?;
        if !retry {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(id).filter(|entry| entry.version == version) {
                return match &entry.result {
                    Ok(info) => Ok(info.clone()),
                    Err(error) => bail!("cached probe failure (use Retry probe): {error}"),
                };
            }
        }
        let result = self.probe(path, &version, sidecars).await;
        if let Err(error) = &result { warn!(file_id = id, "media probe failed: {error:#}"); }
        self.entries.lock().await.insert(id.to_string(), Cached {
            version,
            result: result.as_ref().map(Clone::clone).map_err(|error| format!("{error:#}")),
        });
        result
    }

    async fn probe(&self, path: &Path, version: &str, sidecars: Vec<PathBuf>) -> Result<MediaInfo> {
        let mut command = Command::new("ffprobe");
        command.args(["-v", "error", "-show_streams", "-show_format", "-of", "json"]).arg(path);
        let probe: Probe = serde_json::from_slice(&self.processes.output(&mut command, 60).await?)
            .context("invalid ffprobe JSON")?;
        let mut info = normalize(probe, version)?;
        for (ordinal, external) in sidecars.into_iter().enumerate() {
            let extension = external.extension().and_then(|s| s.to_str()).context("subtitle extension missing")?
                .to_ascii_lowercase();
            info.subtitle_tracks.push(Track {
                stream_index: -i32::try_from(ordinal)? - 1,
                codec_name: match extension.as_str() { "srt" => "subrip", "vtt" => "webvtt", _ => &extension }.to_string(),
                language: None,
                title: Some(external.file_name().context("subtitle filename missing")?.to_string_lossy().into_owned()),
                is_default: false, is_forced: false, color_transfer: None,
                external_path: Some(external),
            });
        }
        Ok(info)
    }
}

fn normalize(probe: Probe, version: &str) -> Result<MediaInfo> {
    let duration = probe.format.duration.as_deref().context("missing media duration")?.parse::<f64>()?;
    ensure!(duration.is_finite() && duration > 0.0, "media duration must be finite and positive");
    let start_time = probe.format.start_time.as_deref().unwrap_or("0").parse::<f64>()?;
    ensure!(start_time.is_finite(), "invalid media start time");
    let mut info = MediaInfo {
        source_version: version.to_string(), duration, start_time,
        video_tracks: Vec::new(), audio_tracks: Vec::new(), subtitle_tracks: Vec::new(),
        attachments: Vec::new(),
    };
    for stream in probe.streams {
        if stream.disposition.get("attached_pic") == Some(&1) { continue; }
        if stream.codec_type.as_deref() == Some("attachment") {
            info.attachments.push(Attachment {
                stream_index: stream.index, ordinal: info.attachments.len(),
                font_extension: font_extension(
                    stream.tags.get("filename").map(String::as_str).unwrap_or(""),
                    stream.tags.get("mimetype").map(String::as_str)),
            });
            continue;
        }
        let tracks = match stream.codec_type.as_deref() {
            Some("video") => &mut info.video_tracks,
            Some("audio") => &mut info.audio_tracks,
            Some("subtitle") => &mut info.subtitle_tracks,
            _ => continue,
        };
        tracks.push(Track {
            stream_index: stream.index,
            codec_name: stream.codec_name.unwrap_or_else(|| "unknown".to_string()),
            language: stream.tags.get("language").cloned(),
            title: stream.tags.get("title").cloned(),
            is_default: stream.disposition.get("default") == Some(&1),
            is_forced: stream.disposition.get("forced") == Some(&1),
            color_transfer: stream.color_transfer, external_path: None,
        });
    }
    ensure!(!info.video_tracks.is_empty(), "media has no playable video stream");
    Ok(info)
}

fn font_extension(name: &str, mime: Option<&str>) -> Option<String> {
    let extension = Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(extension.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2") {
        Some(extension)
    } else if matches!(mime, Some("application/x-truetype-font" | "application/vnd.ms-opentype")) {
        Some("ttf".to_string())
    } else { None }
}

async fn sidecars(path: &Path, root: &Path) -> Result<Vec<PathBuf>> {
    let stem = path.file_stem().context("media filename missing")?.to_string_lossy();
    let mut entries = tokio::fs::read_dir(path.parent().context("media directory missing")?).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&format!("{stem}.")) { continue; }
        let candidate = entry.path();
        let extension = candidate.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        if !matches!(extension.as_str(), "ass" | "ssa" | "srt" | "vtt") { continue; }
        let resolved = tokio::fs::canonicalize(candidate).await?;
        ensure!(resolved.starts_with(root), "subtitle resolves outside media root");
        if tokio::fs::metadata(&resolved).await?.is_file() { paths.push(resolved); }
    }
    paths.sort();
    Ok(paths)
}

async fn source_version(path: &Path, sidecars: &[PathBuf]) -> Result<String> {
    let mut digest = Md5::new();
    for path in std::iter::once(path).chain(sidecars.iter().map(PathBuf::as_path)) {
        let metadata = tokio::fs::metadata(path).await?;
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(metadata.len().to_le_bytes());
        digest.update(metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos().to_le_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fonts_are_extracted_as_fonts() {
        assert_eq!(font_extension("../../font.OTF", None), Some("otf".to_string()));
        assert_eq!(font_extension("attachment", Some("application/x-truetype-font")), Some("ttf".to_string()));
        assert_eq!(font_extension("cover.jpg", Some("image/jpeg")), None);
    }

    #[tokio::test]
    async fn sidecar_changes_invalidate_the_source_version() {
        let root = std::env::temp_dir().join(format!("anime-probe-{}", super::super::sessions::session_id().unwrap()));
        tokio::fs::create_dir(&root).await.unwrap();
        let source = root.join("video.mkv");
        let subtitle = root.join("video.eng.srt");
        tokio::fs::write(&source, b"video").await.unwrap();
        let original = source_version(&source, &[]).await.unwrap();
        tokio::fs::write(&subtitle, b"subtitle").await.unwrap();
        let files = sidecars(&source, &root).await.unwrap();
        assert_eq!(files, vec![subtitle.clone()]);
        let added = source_version(&source, &files).await.unwrap();
        assert_ne!(original, added);
        tokio::fs::write(&subtitle, b"changed subtitle").await.unwrap();
        assert_ne!(added, source_version(&source, &files).await.unwrap());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
