
// backend/src/video/models.rs
//! 定义视频模块中使用的核心数据模型


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
    pub(crate) src_file_pre16mib_md5: String,
    pub(crate) video_stream: i32,
    pub(crate) audio_stream: Option<i32>,
    pub(crate) subtitle_stream: Option<i32>,
    pub(crate) encoder_profile: String,
}

impl PlaybackVariantConfig {
    // 字段声明顺序和显式 null 都参与哈希, 不能排序字段或省略空音轨与字幕.
    pub(crate) fn key(&self) -> Result<String> {
        Ok(format!("{:x}", Md5::digest(serde_json::to_vec(self)?)))
    }
}


/// 服务端构造的单次播放执行参数.
///
/// `new(id, source, info, request, saved_position)` 校验指纹, 轨道和目标位置,
/// 合并历史进度与请求意图, 再交给 `SessionManager::create`.
/// 源文件地址, 目标位置和媒体处理细节不参与缓存身份计算.
///
/// 单次任务的执行参数, 只有 config 参与缓存身份计算, 其余字段用于媒体处理和调度.
#[derive(Clone)]
pub(crate) struct EncodeSpec {
    pub(crate) config: PlaybackVariantConfig,
    pub(super) source: PathBuf,
    pub(crate) duration: f64,
    // 容器时间轴原点可能为负数, 视频, 音频和字幕都要按同一原点归零.
    pub(super) source_start: f64,
    pub(crate) info: MediaInfo,
    pub(super) subtitle: Option<Track>,
    pub(super) hdr: bool,
    pub(super) target: f64,
}

impl EncodeSpec {
    pub(crate) fn new(id: &str, source: PathBuf, info: MediaInfo, request: PlaybackRequest,
        saved_position: Option<f64>) -> Result<Self> {
        ensure!(crate::utils::check_md5_ok(id), "invalid source file ID");
        let saved = saved_position.unwrap_or(0.0);
        ensure!(saved.is_finite() && saved >= 0.0, "invalid saved playback position");
        // 保留原有体验: 已到结尾的观看记录从头播放, 显式换轨位置则严格校验.
        let target = request.position.unwrap_or(if saved < info.duration - 2.0 { saved } else { 0.0 });
        ensure!(target.is_finite() && target >= 0.0 && target < info.duration, "position must be within the video duration");
        let video = info.video_tracks.first().context("media has no playable video stream")?;
        let audio_index = request.audio_stream_index.unwrap_or_else(|| {
            info.audio_tracks.iter().find(|track| track.is_default).or(info.audio_tracks.first()).map(|track| track.stream_index)
        });
        let subtitle_index = request.subtitle_stream_index.unwrap_or_else(|| {
            info.subtitle_tracks.iter().find(|track| track.is_default || track.is_forced).map(|track| track.stream_index)
        });
        let audio = select(&info.audio_tracks, audio_index, "audio")?;
        let subtitle = select(&info.subtitle_tracks, subtitle_index, "subtitle")?;
        let config = PlaybackVariantConfig {
            src_file_pre16mib_md5   : id.to_string(),
            video_stream            : video.stream_index,
            audio_stream            : audio.map(|track| track.stream_index),
            subtitle_stream         : subtitle.as_ref().map(|track| track.stream_index),
            encoder_profile         : ENCODER_PROFILE.to_string(),
        };
        Ok(Self {
            config, source, duration: info.duration, source_start: info.start_time,
            hdr: video.is_hdr(), info, subtitle, target,
        })
    }
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}
