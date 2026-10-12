//! 探测并缓存媒体轨道元数据.

use std::{collections::HashMap, path::Path, time::{Duration, SystemTime}};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::models::{Attachment, MediaInfo, Track};
use crate::video::Processes;
use crate::{db, utils};
use super::CacheManager;


// MetadataEntry
pub(super) struct MetadataEntry {
    info: MediaInfo,
    last_access: SystemTime,
}


impl CacheManager {

    // retry 绕过成功缓存, 失败结果不写入 Map.
    pub(crate) async fn media_info(&self, id: &str, retry: bool) -> Result<MediaInfo> {
        ensure!(utils::check_md5_ok(id), "invalid source file ID");
        let now = SystemTime::now();
        {
            let mut entries = self.inner.metadata.lock().await;
            prune_metadata(&mut entries, now);
            if !retry {
                if let Some(entry) = entries.get_mut(id) {
                    entry.last_access = now;
                    return Ok(entry.info.clone());
                }
            }
        }

        let path = db::get_a_video_path_by_id(&self.inner.database, id).await?
            .context("video file not found")?;
        let info = probe(&path, &self.inner.processes).await?;
        let mut entries = self.inner.metadata.lock().await;
        entries.insert(id.to_string(), MetadataEntry { info: info.clone(), last_access: now });
        prune_metadata(&mut entries, SystemTime::now());
        Ok(info)
    }
}


fn prune_metadata(entries: &mut HashMap<String, MetadataEntry>, now: SystemTime) {
    entries.retain(|_, entry| now.duration_since(entry.last_access).unwrap_or_default() < Duration::from_secs(3600));
    while entries.len() > 1000 {
        let Some(oldest) = entries.iter().min_by_key(|(_, entry)| entry.last_access).map(|(key, _)| key.clone()) else {
            break;
        };
        entries.remove(&oldest);
    }
}

/// 解析媒体文件并归一化 ffprobe 结果.
async fn probe(path: &Path, processes: &Processes) -> Result<MediaInfo> {

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

        // 只推导允许的字体扩展名, 不把容器附件中的原始文件名用作输出路径.
        fn font_extension(name: &str, mime: Option<&str>) -> Option<String> {
            let extension = Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(extension.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2") {
                Some(extension)
            } else if matches!(mime, Some("application/x-truetype-font" | "application/vnd.ms-opentype")) {
                Some("ttf".to_string())
            } else { None }
        }

        let mut command = Command::new("ffprobe");
        command.args(["-v", "error", "-show_streams", "-show_format", "-of", "json"]).arg(path);
        let probe: Probe = serde_json::from_slice(&processes.output(&mut command, 60).await?)?;

        let duration = probe.format.duration.as_deref().context("missing media duration")?.parse::<f64>()?;
        anyhow::ensure!(duration.is_finite() && duration > 0.0, "media duration must be finite and positive");
        let start_time = probe.format.start_time.as_deref().unwrap_or("0").parse::<f64>()?;
        anyhow::ensure!(start_time.is_finite(), "invalid media start time");

        // Matroska 的 duration 是包含正起点偏移的结束时间, 必须减去偏移才得到实际播放时长.
        let duration = if probe.format.format_name.split(',').any(|name| name == "matroska" || name == "webm") {
            duration - start_time.max(0.0)
        } else { duration };
        anyhow::ensure!(duration > 0.0, "media has no duration after its start timestamp");
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
                    ordinal: info.attachments.len(),
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
        anyhow::ensure!(!info.video_tracks.is_empty(), "media has no playable video stream");
        Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_info() -> MediaInfo {
        MediaInfo {
            duration: 1.0,
            start_time: 0.0,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn metadata_pruning_expires_old_entries_and_caps_capacity() {
        let now = SystemTime::now();
        let mut entries = HashMap::new();
        entries.insert("expired".to_string(), MetadataEntry {
            info: media_info(),
            last_access: now - Duration::from_secs(3601),
        });
        for index in 0..1001 {
            entries.insert(format!("item-{index}"), MetadataEntry {
                info: media_info(),
                last_access: now - Duration::from_secs(index),
            });
        }

        prune_metadata(&mut entries, now);

        assert_eq!(entries.len(), 1000);
        assert!(!entries.contains_key("expired"));
        assert!(!entries.contains_key("item-1000"));
    }
}
