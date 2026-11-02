

// backend/src/models.rs
// 定义媒体索引, 播放请求和稳定缓存身份.

use std::path::{Path, PathBuf};

use anyhow::{ensure, Result};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::{constants::HASH_LIMIT, utils};

mod media;

#[cfg(test)]
#[path = "../tests/models.rs"]
mod tests;

pub(crate) use media::{Attachment, MediaConfigs, MediaMetadata, StreamInfo};


/// 保存一次文件扫描得到的内容身份.
#[derive(Debug)]
pub(crate) struct ScannedFile {
    /// 文件前 16 MiB 的 MD5 内容标识.
    pub(crate) media_id: String,
    /// 已通过媒体目录边界检查的绝对路径.
    pub(crate) path: PathBuf,
}

impl ScannedFile {
    /// 读取媒体文件前缀并计算内容标识, 由扫描器放入阻塞任务执行.
    pub(crate) fn new(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute() && path.to_str().is_some()
            && !path.components().any(|part| matches!(part, std::path::Component::ParentDir)),
            "invalid absolute media path: {}", path.display());
        ensure!(std::fs::metadata(path)?.is_file(), "not a media file: {}", path.display());
        let media_id = utils::md5_hash_prefix(path, HASH_LIMIT).map_err(anyhow::Error::msg)?;
        Ok(Self { media_id, path: path.to_owned() })
    }
}

/// 汇总一次全量扫描对媒体索引的变更.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ScanChanges {
    /// 新增路径数量.
    pub(crate) added: u64,
    /// 内容身份发生变化的路径数量.
    pub(crate) updated: u64,
    /// 已移除的失效路径数量.
    pub(crate) removed: u64,
    /// 扫描完成后的索引路径总数.
    pub(crate) total: u64,
}

/// 按媒体路径请求构建缓存, 不向后端传递等待位置或编码参数.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackQuery {
    /// 要转码的容器内媒体路径.
    pub(crate) path: PathBuf,
    /// 请求的视频全局流索引, 缺省时使用配置或首条视频流.
    pub(crate) video_stream_index: Option<i32>,
    /// 请求的音频索引, 缺省继承偏好, 显式 null 关闭音频.
    #[serde(default, deserialize_with = "track_choice")]
    pub(crate) audio_stream_index: Option<Option<i32>>,
    /// 请求的字幕索引, 缺省继承偏好, 显式 null 关闭字幕.
    #[serde(default, deserialize_with = "track_choice")]
    pub(crate) subtitle_stream_index: Option<Option<i32>>,
}

/// 以固定字段顺序定义缓存身份和 instance.json 的唯一格式.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CacheSpec {
    /// 源媒体前 16 MiB 的 MD5 标识.
    pub(crate) media_id: String,
    /// 实际使用的视频流索引.
    pub(crate) video_stream_index: i32,
    /// 实际使用的可选音频流索引.
    pub(crate) audio_stream_index: Option<i32>,
    /// 实际使用的可选字幕流索引.
    pub(crate) subtitle_stream_index: Option<i32>,
    /// 缓存和编码契约版本.
    pub(crate) version: String,
}

impl CacheSpec {
    /// 根据已验证的媒体身份和流选择构造缓存描述.
    pub(crate) fn new(
        media_id: String, video_stream_index: i32, audio_stream_index: Option<i32>,
        subtitle_stream_index: Option<i32>, version: String,
    ) -> Self {
        Self { media_id, video_stream_index, audio_stream_index, subtitle_stream_index, version }
    }

    /// 从磁盘实例描述恢复稳定身份, 不依赖源路径仍然存在.
    pub(crate) fn from_descriptor(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// 对固定顺序且保留显式 null 的身份 JSON 计算 MD5.
    pub(crate) fn handle_id(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{:x}", Md5::digest(self.descriptor()?)))
    }

    /// 序列化全部身份字段, 不包含路径, 播放位置或线程数.
    pub(crate) fn descriptor(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// 区分 JSON 中未提供轨道字段与显式 null.
fn track_choice<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(deserializer).map(Some)
}
