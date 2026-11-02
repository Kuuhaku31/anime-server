
// backend/src/process/metadata.rs
//! 按媒体路径运行 ffprobe 并解析真实元数据.

use std::{collections::HashMap, path::Path};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::models::{Attachment, MediaMetadata, StreamInfo};

use super::ProcessManager;

#[cfg(test)]
#[path = "../tests/process/metadata.rs"]
mod tests;

impl ProcessManager {
    /// 在全局进程预算内实时探测媒体, 不保存 metadata 缓存.
    pub(crate) async fn probe(&self, path: &Path) -> Result<MediaMetadata> {
        let mut command = Command::new("ffprobe");
        command.args(["-v", "error", "-show_streams", "-show_format", "-of", "json"]).arg(path);
        parse_probe(&self.output(&mut command, 60).await?)
            .with_context(|| format!("cannot probe media: {}", path.display()))
    }
}

/// 对应 ffprobe 的顶层 JSON 数据.
#[derive(Deserialize)]
struct Probe {
    /// 全部容器流, 包含附件和封面.
    #[serde(default)]
    streams: Vec<ProbeStream>,
    /// 容器时间轴和格式.
    format: ProbeFormat,
}

/// 对应 ffprobe 的容器信息.
#[derive(Deserialize)]
struct ProbeFormat {
    /// 容器报告的时长文本.
    duration: Option<String>,
    /// 容器报告的时间轴起点.
    start_time: Option<String>,
    /// 逗号分隔的格式名称.
    format_name: String,
}

/// 对应 ffprobe 的单条原始流信息.
#[derive(Deserialize)]
struct ProbeStream {
    /// 容器内全局流索引.
    index: i32,
    /// 视频, 音频, 字幕或附件类型.
    codec_type: Option<String>,
    /// 流的编解码器名称.
    codec_name: Option<String>,
    /// 视频色彩传递函数.
    color_transfer: Option<String>,
    /// 语言, 标题和附件类型标签.
    #[serde(default)]
    tags: HashMap<String, String>,
    /// 默认, 强制和封面等标记.
    #[serde(default)]
    disposition: HashMap<String, i32>,
}

/// 将探测结果转换为共享模型, 同时保留内部字幕和 HDR 所需信息.
fn parse_probe(bytes: &[u8]) -> Result<MediaMetadata> {
    let probe: Probe = serde_json::from_slice(bytes)?;
    let duration = probe.format.duration.as_deref().context("missing media duration")?.parse::<f64>()?;
    let start_time = probe.format.start_time.as_deref().unwrap_or("0").parse::<f64>()?;
    ensure!(duration.is_finite() && duration > 0.0 && start_time.is_finite(), "invalid media timeline");
    ensure!(!probe.format.format_name.is_empty(), "missing media container format");
    // Matroska 的 duration 包含正起点偏移, 实际时长需要减去这部分偏移.
    let duration = if probe.format.format_name.split(',').any(|name| matches!(name, "matroska" | "webm")) {
        duration - start_time.max(0.0)
    } else { duration };
    ensure!(duration > 0.0, "media has no duration after its start timestamp");
    let mut metadata = MediaMetadata {
        duration, start_time, format: probe.format.format_name,
        video_streams: Vec::new(), audio_streams: Vec::new(), subtitle_streams: Vec::new(),
        attachments: Vec::new(),
    };
    for stream in probe.streams {
        ensure!(stream.index >= 0, "invalid media stream index");
        if stream.disposition.get("attached_pic") == Some(&1) { continue; }
        if stream.codec_type.as_deref() == Some("attachment") {
            metadata.attachments.push(Attachment {
                ordinal: metadata.attachments.len(),
                font_extension: font_extension(
                    stream.tags.get("filename").map(String::as_str).unwrap_or(""),
                    stream.tags.get("mimetype").map(String::as_str),
                ),
            });
            continue;
        }
        let streams = match stream.codec_type.as_deref() {
            Some("video") => &mut metadata.video_streams,
            Some("audio") => &mut metadata.audio_streams,
            Some("subtitle") => &mut metadata.subtitle_streams,
            _ => continue,
        };
        streams.push(StreamInfo {
            stream_index: stream.index,
            codec_name: stream.codec_name.context("missing media stream codec")?,
            language: stream.tags.get("language").cloned(),
            title: stream.tags.get("title").cloned(),
            is_default: stream.disposition.get("default") == Some(&1),
            is_forced: stream.disposition.get("forced") == Some(&1),
            color_transfer: stream.color_transfer,
        });
    }
    ensure!(!metadata.video_streams.is_empty(), "media has no playable video stream");
    Ok(metadata)
}

/// 从附件标签推导允许的字体扩展名, 不使用附件原名作为写入路径.
fn font_extension(name: &str, mime: Option<&str>) -> Option<String> {
    let extension = Path::new(name).extension().and_then(|value| value.to_str())
        .unwrap_or("").to_ascii_lowercase();
    if matches!(extension.as_str(), "ttf" | "otf" | "ttc" | "woff" | "woff2") {
        Some(extension)
    } else if matches!(mime, Some("application/x-truetype-font" | "application/vnd.ms-opentype")) {
        Some("ttf".to_string())
    } else { None }
}
