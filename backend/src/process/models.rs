//! 定义媒体编码执行参数及轨道选择.

use anyhow::{Context, Result};
use crate::models::{MediaInfo, PlaybackVariantConfig, Track};
use std::path::PathBuf;

/// 服务端构造的单次播放执行参数.
///
/// 单次任务的执行参数, 只有 config 参与缓存身份计算, 其余字段用于媒体处理和调度.
#[derive(Clone)]
pub(crate) struct EncodeSpec {
    pub(crate) config: PlaybackVariantConfig,
    pub(super) source: PathBuf,
    pub(crate) duration: f64,
    // 容器时间轴原点可能为负数, 视频, 音频和字幕都要按同一原点归零.
    pub(super) source_start: f64,
    pub(super) info: MediaInfo,
    pub(crate) subtitle: Option<Track>,
    pub(super) hdr: bool,
}

impl EncodeSpec {
    pub(crate) fn from_config(config: PlaybackVariantConfig, source: PathBuf, info: MediaInfo) -> Result<Self> {
        let video = info.video_tracks.iter().find(|track| track.stream_index == config.video_stream())
            .context("configured video track does not exist")?;
        select(&info.audio_tracks, config.audio_stream(), "audio")?;
        let subtitle = select(&info.subtitle_tracks, config.selection().1, "subtitle")?;
        Ok(Self {
            config, source, duration: info.duration, source_start: info.start_time,
            hdr: video.is_hdr(), info, subtitle,
        })
    }
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}
