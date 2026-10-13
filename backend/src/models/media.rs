
// backend/src/models/media.rs
//! 媒体探测和播放配置相关数据结构.

use md5::{Digest, Md5};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::models::PlaybackRequest;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Track {
    pub(crate) stream_index: i32,
    pub(crate) codec_name: String,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
    #[serde(skip_serializing)]
    pub(crate) color_transfer: Option<String>,
}

impl Track {
    pub(crate) fn is_bitmap(&self) -> bool {
        matches!(self.codec_name.as_str(), "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
    }

    pub(crate) fn is_hdr(&self) -> bool {
        matches!(self.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Attachment {
    pub(crate) ordinal: usize,
    pub(crate) font_extension: Option<String>,
}


/// 媒体探测结果, 可序列化为视频, 音频和字幕轨道信息.
///
/// 轨道选择使用容器内全局流索引. 时间轴原点和字体附件只供模块内部处理,
/// 不随 API 响应公开, 调用方无需自行解析 ffprobe 输出.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MediaInfo {
    pub(crate) duration: f64,
    #[serde(skip_serializing)]
    pub(crate) start_time: f64,
    pub(crate) video_tracks: Vec<Track>,
    pub(crate) audio_tracks: Vec<Track>,
    pub(crate) subtitle_tracks: Vec<Track>,
    #[serde(skip_serializing)]
    pub(crate) attachments: Vec<Attachment>,
}


/// 稳定的缓存身份配置, 不等同于客户端播放请求.
///
/// `variant_key()` 按字段声明顺序序列化, 保留可选轨道的 null, 再计算 MD5.
/// 配置只包含源文件指纹, 视频/音频/字幕流索引和编码契约.
/// 不应将文件路径, 会话 ID, 播放位置或动态线程数加入配置.
/// 缓存身份只由源文件指纹, 全局流索引和编码契约决定, 不包含路径或播放位置.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PlaybackVariantConfig {

    src_file_pre16mib_md5: String,
    video_stream: i32,
    audio_stream: Option<i32>,
    subtitle_stream: Option<i32>,
    encoder_profile: String,

    #[serde(skip)]
    variant_key: String,
}

fn select(tracks: &[Track], index: Option<i32>, kind: &str) -> Result<Option<Track>> {
    index.map(|index| tracks.iter().find(|track| track.stream_index == index).cloned()
        .with_context(|| format!("selected {kind} track does not exist"))).transpose()
}

impl PlaybackVariantConfig {

    /// 根据媒体信息和播放请求固定所选轨道及编码契约.
    pub(crate) fn new(file_id: &str, info: &MediaInfo, request: &PlaybackRequest) -> Result<Self> {
        ensure!(crate::utils::check_md5_ok(file_id), "invalid source file ID");
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
        let config = Self {
            src_file_pre16mib_md5: file_id.to_string(),
            video_stream: video.stream_index,
            audio_stream: audio.map(|track| track.stream_index),
            subtitle_stream: subtitle.as_ref().map(|track| track.stream_index),
            encoder_profile: crate::constants::ENCODER_PROFILE.to_string(),
            variant_key: String::new(), // 临时占位
        };
        // 字段声明顺序和显式 null 都参与哈希, 不能排序字段或省略空音轨与字幕.
        let variant_key = format!("{:x}", Md5::digest(serde_json::to_vec(&config)?));
        Ok(Self { variant_key, ..config })
    }

    // getters

    pub(crate) fn variant_key(&self) -> &str { &self.variant_key }
    pub(crate) fn video_stream(&self) -> i32 { self.video_stream }
    pub(crate) fn audio_stream(&self) -> Option<i32> { self.audio_stream }
    pub(crate) fn src_file_pre16mib_md5(&self) -> &str { &self.src_file_pre16mib_md5 }
    pub(crate) fn selection(&self) -> (Option<i32>, Option<i32>) { (self.audio_stream, self.subtitle_stream) }
}
