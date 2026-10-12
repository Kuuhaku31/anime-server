//! 定义视频编码任务数据模型.
use anyhow::{Context, Result, ensure};
use crate::models::{MediaInfo, PlaybackRequest, PlaybackVariantConfig, Track};
use std::path::PathBuf;

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

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}

impl EncodeSpec {
    pub(super) fn from_config(config: PlaybackVariantConfig, source: PathBuf, info: MediaInfo, target: f64) -> Result<Self> {
        let video = info.video_tracks.iter().find(|track| track.stream_index == config.video_stream())
            .context("configured video track does not exist")?;
        select(&info.audio_tracks, config.audio_stream(), "audio")?;
        let subtitle = select(&info.subtitle_tracks, config.selection().1, "subtitle")?;
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
