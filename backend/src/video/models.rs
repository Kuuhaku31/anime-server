//! 定义视频播放配置和编码任务数据模型.
use md5::{Digest, Md5};
use serde::Serialize;
use anyhow::{Context, Result, ensure};

use crate::models::PlaybackRequest;
use std::path::PathBuf;

use super::{cache::ENCODER_PROFILE, metadata::{MediaInfo, Track}};



/// 稳定的缓存身份配置, 不等同于客户端播放请求.
///
/// `key()` 按字段声明顺序序列化, 保留可选轨道的 null, 再计算 MD5.
/// 配置只包含源文件指纹, 视频/音频/字幕流索引和编码契约.
/// 不应将文件路径, 会话 ID, 播放位置或动态线程数加入配置.
/// 缓存身份只由源文件指纹, 全局流索引和编码契约决定, 不包含路径或播放位置.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PlaybackVariantConfig {
    pub(super) src_file_pre16mib_md5: String,
    pub(super) video_stream: i32,
    pub(super) audio_stream: Option<i32>,
    pub(super) subtitle_stream: Option<i32>,
    pub(super) encoder_profile: String,
}

impl PlaybackVariantConfig {
    /// 根据媒体信息和播放请求固定所选轨道及编码契约.
    pub(crate) fn new(id: &str, info: &MediaInfo, request: &PlaybackRequest) -> Result<Self> {
        ensure!(crate::utils::check_md5_ok(id), "invalid source file ID");
        let video = info.video_tracks.first().context("media has no playable video stream")?;
        let audio_index = request.audio_stream_index.unwrap_or_else(|| {
            info.audio_tracks.iter().find(|track| track.is_default)
                .or(info.audio_tracks.first()).map(|track| track.stream_index)
        });
        let subtitle_index = request.subtitle_stream_index.unwrap_or_else(|| {
            info.subtitle_tracks.iter().find(|track| track.is_default || track.is_forced)
                .map(|track| track.stream_index)
        });
        let audio = select(&info.audio_tracks, audio_index, "audio")?;
        let subtitle = select(&info.subtitle_tracks, subtitle_index, "subtitle")?;
        Ok(Self {
            src_file_pre16mib_md5: id.to_string(),
            video_stream: video.stream_index,
            audio_stream: audio.map(|track| track.stream_index),
            subtitle_stream: subtitle.as_ref().map(|track| track.stream_index),
            encoder_profile: ENCODER_PROFILE.to_string(),
        })
    }

    // 字段声明顺序和显式 null 都参与哈希, 不能排序字段或省略空音轨与字幕.
    pub(crate) fn key(&self) -> Result<String> {
        Ok(format!("{:x}", Md5::digest(serde_json::to_vec(self)?)))
    }

    pub(crate) fn selection(&self) -> (Option<i32>, Option<i32>) {
        (self.audio_stream, self.subtitle_stream)
    }
}


/// 服务端构造的单次播放执行参数.
///
/// 单次任务的执行参数, 只有 config 参与缓存身份计算, 其余字段用于媒体处理和调度.
#[derive(Clone)]
pub(crate) struct EncodeSpec {
    pub(super) config: PlaybackVariantConfig,
    pub(super) source: PathBuf,
    pub(super) duration: f64,
    // 容器时间轴原点可能为负数, 视频, 音频和字幕都要按同一原点归零.
    pub(super) source_start: f64,
    pub(super) info: MediaInfo,
    pub(super) subtitle: Option<Track>,
    pub(super) hdr: bool,
    pub(super) target: f64,
}

impl EncodeSpec {
    pub(super) fn from_config(config: PlaybackVariantConfig, source: PathBuf, info: MediaInfo, target: f64) -> Result<Self> {
        let video = info.video_tracks.iter().find(|track| track.stream_index == config.video_stream)
            .context("configured video track does not exist")?;
        select(&info.audio_tracks, config.audio_stream, "audio")?;
        let subtitle = select(&info.subtitle_tracks, config.subtitle_stream, "subtitle")?;
        ensure!(target.is_finite() && target >= 0.0 && target < info.duration, "position must be within the video duration");
        Ok(Self {
            config, source, duration: info.duration, source_start: info.start_time,
            hdr: video.is_hdr(), info, subtitle, target,
        })
    }
}

/// 合并显式位置和历史位置, 保持已看完的视频从头开始播放.
pub(crate) fn playback_target(info: &MediaInfo, request: &PlaybackRequest) -> Result<f64> {
    let saved = request.saved_position().unwrap_or(0.0);
    ensure!(saved.is_finite() && saved >= 0.0, "invalid saved playback position");
    let target = request.position.unwrap_or(if saved < info.duration - 2.0 { saved } else { 0.0 });
    ensure!(target.is_finite() && target >= 0.0 && target < info.duration, "position must be within the video duration");
    Ok(target)
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}


// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    fn media_info() -> MediaInfo {
        serde_json::from_value(serde_json::json!({
            "duration": 20.0,
            "start_time": 0.0,
            "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
            "audio_tracks": [{"stream_index": 1, "codec_name": "aac", "is_default": true, "is_forced": false}],
            "subtitle_tracks": [{"stream_index": 3, "codec_name": "ass", "is_default": false, "is_forced": false}],
            "attachments": []
        })).unwrap()
    }

    #[test]
    fn variant_key_uses_selected_tracks_but_not_playback_position() {
        let info = media_info();
        let first: PlaybackRequest = serde_json::from_str(r#"{"position":2.5,"subtitle_stream_index":3}"#).unwrap();
        let second: PlaybackRequest = serde_json::from_str(r#"{"position":18.0,"subtitle_stream_index":3}"#).unwrap();
        let first = PlaybackVariantConfig::new(&"a".repeat(32), &info, &first).unwrap();
        let second = PlaybackVariantConfig::new(&"a".repeat(32), &info, &second).unwrap();

        assert_eq!(first.key().unwrap(), second.key().unwrap());
        assert_eq!(first.selection(), (Some(1), Some(3)));
        assert!(serde_json::to_string(&first).unwrap().contains("\"audio_stream\":1,\"subtitle_stream\":3"));
    }

    #[test]
    fn disabled_optional_tracks_remain_explicit_nulls_in_key_data() {
        let info = media_info();
        let request: PlaybackRequest = serde_json::from_str(r#"{"audio_stream_index":null,"subtitle_stream_index":null}"#).unwrap();
        let config = PlaybackVariantConfig::new(&"a".repeat(32), &info, &request).unwrap();

        assert_eq!(config.selection(), (None, None));
        assert!(serde_json::to_string(&config).unwrap().contains("\"audio_stream\":null,\"subtitle_stream\":null"));
    }
}
