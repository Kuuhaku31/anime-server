
// backend/src/models/media.rs
//! 定义实时媒体元数据与数据库保存的播放配置.

use serde::{Deserialize, Serialize};

/// 描述一个容器内的媒体流.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamInfo {
    /// 容器内全局流索引, 不是数组下标.
    pub(crate) stream_index: i32,
    /// 流的编解码器名称.
    pub(crate) codec_name: String,
    /// 可选语言标签.
    pub(crate) language: Option<String>,
    /// 可选轨道标题.
    pub(crate) title: Option<String>,
    /// 是否为容器默认流.
    pub(crate) is_default: bool,
    /// 是否为强制字幕流.
    pub(crate) is_forced: bool,
    /// 仅供内部 HDR 转换使用的传递函数.
    #[serde(skip)]
    pub(crate) color_transfer: Option<String>,
}

impl StreamInfo {
    /// 判断字幕是否需要按位图叠加而不是文本烧录.
    pub(crate) fn is_bitmap(&self) -> bool {
        matches!(self.codec_name.as_str(), "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
    }

    /// 判断视频是否需要 HDR 到 SDR 色调映射.
    pub(crate) fn is_hdr(&self) -> bool {
        matches!(self.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67"))
    }
}

/// 描述字幕渲染可使用的容器附件.
#[derive(Debug, Clone)]
pub(crate) struct Attachment {
    /// 附件在容器附件流中的顺序编号.
    pub(crate) ordinal: usize,
    /// 通过白名单检查的字体扩展名, 非字体附件为 None.
    pub(crate) font_extension: Option<String>,
}

/// 保存 ProcessManager 按路径实时探测的媒体信息.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MediaMetadata {
    /// 归一化时间轴上的有效播放时长.
    pub(crate) duration: f64,
    /// ffprobe 返回的容器格式名称.
    pub(crate) format: String,
    /// 排除封面附件后的可播放视频流.
    pub(crate) video_streams: Vec<StreamInfo>,
    /// 可选择的音频流.
    pub(crate) audio_streams: Vec<StreamInfo>,
    /// 可选择的内嵌字幕流.
    pub(crate) subtitle_streams: Vec<StreamInfo>,
    /// 供内部音视频时间轴归零使用的容器起点.
    #[serde(skip)]
    pub(crate) start_time: f64,
    /// 供内部字幕渲染使用的字体附件信息.
    #[serde(skip)]
    pub(crate) attachments: Vec<Attachment>,
}

/// 保存同一媒体内容的播放进度和流偏好.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub(crate) struct MediaConfigs {
    /// 上次播放的绝对秒数, None 表示没有记录.
    pub(crate) position: Option<f64>,
    /// 上次选择的视频全局流索引.
    pub(crate) video_stream_index: Option<i32>,
    /// 上次选择的可选音频全局流索引.
    pub(crate) audio_stream_index: Option<i32>,
    /// 上次选择的可选字幕全局流索引.
    pub(crate) subtitle_stream_index: Option<i32>,
}

// /// 组合前端所需的实时元数据和持久化配置.
// #[derive(Debug, Serialize)]
// pub(crate) struct MediaInfo {
//     /// 当前路径的实时媒体元数据.
//     pub(crate) metadata: MediaMetadata,
//     /// 当前媒体内容身份对应的配置.
//     pub(crate) configs: MediaConfigs,
// }
