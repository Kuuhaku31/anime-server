//! 测试构建共享的媒体夹具, 不访问真实视频也不启动媒体进程.

use crate::models::{MediaInfo, PlaybackRequest};

use super::{fragment::Fragment, CachedMedia, Tail};

/// 测试使用的合法源文件 ID.
pub(crate) const FILE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// 返回 20 秒的单视频, 单音轨, 单 ASS 字幕媒体信息和默认播放请求.
pub(crate) fn fixture() -> (MediaInfo, PlaybackRequest) {
    let info = serde_json::from_value(serde_json::json!({
        "duration": 20.0, "start_time": 0.0,
        "video_tracks": [{"stream_index": 0, "codec_name": "h264", "is_default": true, "is_forced": false}],
        "audio_tracks": [{"stream_index": 1, "codec_name": "aac", "is_default": true, "is_forced": false}],
        "subtitle_tracks": [{"stream_index": 2, "codec_name": "ass", "is_default": false, "is_forced": false}],
        "attachments": []
    })).unwrap();
    (info, PlaybackRequest::default())
}

/// 构造只含时间轴的已发布媒体前缀, 每个分片固定 2 秒, 不包含真实 init.
pub(crate) fn published_media(count: u32, complete: bool) -> CachedMedia {
    let segments = (0..count).map(|index| Fragment {
        start: Tail { video: index as u64 * 180_000, audio: index as u64 * 96_000 },
        end: Tail { video: (index as u64 + 1) * 180_000, audio: (index as u64 + 1) * 96_000 },
        sequence: index + 1,
    }).collect();
    CachedMedia { init: None, segments, complete }
}
