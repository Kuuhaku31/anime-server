//! 探测并缓存媒体轨道元数据.

use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
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
    // 附件流内部的相对编号, 用于 -dump_attachment:t, 不同于全局 stream_index.
    pub ordinal: usize,
    pub font_extension: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
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
    #[serde(default)]
    format_name: String,
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

/// 按文件指纹复用探测成功或失败结果, 显式 Retry probe 才绕过旧结果.
#[derive(Clone)]
pub struct MetadataCache {
    entries: Arc<Mutex<HashMap<String, Result<MediaInfo, String>>>>,
    processes: Processes,
}

impl MetadataCache {
    pub fn new(processes: Processes) -> Self {
        Self { entries: Arc::new(Mutex::new(HashMap::new())), processes }
    }

    pub async fn get(&self, id: &str, path: &Path, retry: bool) -> Result<MediaInfo> {
        if !retry {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(id) {
                return match entry {
                    Ok(info) => Ok(info.clone()),
                    Err(error) => bail!("cached probe failure (use Retry probe): {error}"),
                };
            }
        }
        let result = self.probe(path).await;
        if let Err(error) = &result { warn!(file_id = id, "media probe failed: {error:#}"); }
        self.entries.lock().await.insert(id.to_string(),
            result.as_ref().map(Clone::clone).map_err(|error| format!("{error:#}")));
        result
    }

    async fn probe(&self, path: &Path) -> Result<MediaInfo> {
        let mut command = Command::new("ffprobe");
        command.args(["-v", "error", "-show_streams", "-show_format", "-of", "json"]).arg(path);
        let probe: Probe = serde_json::from_slice(&self.processes.output(&mut command, 60).await?)
            .context("invalid ffprobe JSON")?;
        normalize(probe)
    }
}

fn normalize(probe: Probe) -> Result<MediaInfo> {
    let duration = probe.format.duration.as_deref().context("missing media duration")?.parse::<f64>()?;
    ensure!(duration.is_finite() && duration > 0.0, "media duration must be finite and positive");
    let start_time = probe.format.start_time.as_deref().unwrap_or("0").parse::<f64>()?;
    ensure!(start_time.is_finite(), "invalid media start time");
    // Matroska 的 duration 是包含正起点偏移的结束时间, 必须减去偏移才得到实际播放时长.
    let duration = if probe.format.format_name.split(',').any(|name| name == "matroska" || name == "webm") {
        duration - start_time.max(0.0)
    } else { duration };
    ensure!(duration > 0.0, "media has no duration after its start timestamp");
    let mut info = MediaInfo {
        duration, start_time,
        video_tracks: Vec::new(), audio_tracks: Vec::new(), subtitle_tracks: Vec::new(),
        attachments: Vec::new(),
    };
    for stream in probe.streams {
        // 封面附件可能伪装成视频流, 不参与默认视频轨选择.
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
            color_transfer: stream.color_transfer,
        });
    }
    ensure!(!info.video_tracks.is_empty(), "media has no playable video stream");
    Ok(info)
}

// 只推导允许的字体扩展名, 不把容器附件中的原始文件名用作输出路径.
fn font_extension(name: &str, mime: Option<&str>) -> Option<String> {
    let extension = Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(extension.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2") {
        Some(extension)
    } else if matches!(mime, Some("application/x-truetype-font" | "application/vnd.ms-opentype")) {
        Some("ttf".to_string())
    } else { None }
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

    #[test]
    fn only_container_stream_indexes_are_exposed() {
        let probe = serde_json::from_value(serde_json::json!({
            "format": {"duration": "25", "start_time": "5", "format_name": "matroska,webm"},
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264"},
                {"index": 3, "codec_type": "subtitle", "codec_name": "ass"},
                {"index": 4, "codec_type": "attachment", "tags": {"filename": "font.ttf"}}
            ]
        })).unwrap();
        let info = normalize(probe).unwrap();
        assert_eq!(info.start_time, 5.0);
        assert_eq!(info.duration, 20.0);
        assert_eq!(info.subtitle_tracks.len(), 1);
        assert_eq!(info.subtitle_tracks[0].stream_index, 3);
        assert_eq!(info.attachments.len(), 1);
        let public = serde_json::to_value(info).unwrap();
        assert!(public.get("attachments").is_none());
        assert!(public.get("source_version").is_none());
    }
}
