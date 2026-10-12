
// backend/src/models.rs
//! 定义数据库记录和接口数据结构.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{constants::HASH_LIMIT, utils, video::{MediaInfo, SessionStatus}};


#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct VideoFile {
    pub(crate) pre16mib_md5: String,
    pub(crate) name: String,
    pub(crate) size: i64,
    pub(crate) audio_stream_index: Option<i32>,
    pub(crate) subtitle_stream_index: Option<i32>,
    pub(crate) ddp_epi_id: Option<i64>,
    danmaku_offset: f64,
    pub(crate) watch_pos: Option<f64>,
    watched_datetime: Option<String>,
    pub(crate) path: String,
}


#[derive(Debug, Deserialize)]
pub(crate) struct ProgressInput {
    pub(crate) position: f64,
}

/// 进度按文件指纹保存原视频绝对秒数, 跨路径副本和不同播放配置复用.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct WatchProgress {
    pre16mib_md5: String,
    pub(crate) watch_pos: Option<f64>,
    watched_datetime: Option<String>,
    pub(crate) audio_stream_index: Option<i32>,
    pub(crate) subtitle_stream_index: Option<i32>,
}


// 数据库操作对象

/// 扫描文件夹得到的文件实例
#[derive(Debug)]
pub(crate) struct ScannedFile {
    pre16mib_md5: String,
    size: i64,
    path: PathBuf, // 绝对地址
}

impl ScannedFile {

    pub(crate) fn new(path: &Path) -> Result<Self, String> {
        // 地址始终为完整绝对文件地址, 不规范化符号链接以保留原路径身份.
        if !path.is_absolute() || path.to_str().is_none()
            || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("invalid absolute file path: {}", path.display()));
        }
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() { return Err(format!("not a file: {}", path.display())); }
        let path_buf = path.to_path_buf();
        let hash = utils::md5_hash_prefix(&path_buf, HASH_LIMIT)?;
        Ok(Self {
            pre16mib_md5: hash,
            size: metadata.len() as i64,
            path: path_buf,
        })
    }

    // getters
    pub(crate) fn pre16mib_md5      (&self) -> &str { &self.pre16mib_md5 }
    pub(crate) fn size              (&self) -> i64 { self.size }
    pub(crate) fn path              (&self) -> &PathBuf { &self.path }
}


pub(crate) struct ScanChanges {
    pub(crate) added: u64,
    pub(crate) removed: u64,
    pub(crate) total: u64, // 所有现存视频路径数
}


// ========================================================================== //

/// 客户端选轨和位置请求, 不接受客户端提供的完整编码配置.
///
/// 轨道字段缺省表示使用数据库偏好或服务端默认选择, 显式 null 表示关闭.
/// `with_preferences(audio, subtitle)` 只补充缺省字段, 保留显式选择.
/// 位置字段由反序列化读取, 最终合法性由 `EncodeSpec::new` 检查.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackRequest {
    // 缺省表示服务端选轨, 显式 null 表示关闭, 两者不能混为一谈.
    #[serde(default, deserialize_with = "track_choice")]
    pub(crate) audio_stream_index: Option<Option<i32>>,
    #[serde(default, deserialize_with = "track_choice")]
    pub(crate) subtitle_stream_index: Option<Option<i32>>,
    pub(crate) position: Option<f64>,
}

/// 创建播放会话的响应, 包含状态快照, 媒体信息和实际选中的轨道.
///
/// 状态字段通过 Serde flatten 展开, 不向客户端返回缓存键或执行参数.
impl PlaybackRequest {
    /// 仅将缺省字段补为数据库偏好, 显式 null 仍表示本次关闭对应轨道.
    pub(crate) fn with_preferences(mut self, audio: Option<i32>, subtitle: Option<i32>) -> Self {
        if self.audio_stream_index.is_none() { self.audio_stream_index = Some(audio); }
        if self.subtitle_stream_index.is_none() { self.subtitle_stream_index = Some(subtitle); }
        self
    }
}

// 为显式 null 保留外层 Some, 与未提供字段时的 None 区分.
fn track_choice<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(deserializer).map(Some)
}

/// 播放响应中的实际音轨和字幕选择, 仅通过序列化向客户端提供.
#[derive(Debug, Serialize)]
struct Selection {
    audio: Option<i32>,
    subtitle: Option<i32>,
}

/// 创建播放会话的响应, 包含展开的状态快照, 媒体信息和实际轨道选择.
#[derive(Debug, Serialize)]
pub(crate) struct PlaybackStart {
    #[serde(flatten)]
    // 会话状态快照.
    session: SessionStatus,
    // 媒体信息, 播放开始时间等.
    media: MediaInfo,
    // 实际音轨和字幕选择.
    selection: Selection,
}

impl PlaybackStart {
    /// 组装播放响应, 保持响应字段私有及原有 JSON 结构.
    pub(crate) fn new(session: SessionStatus, media: MediaInfo, audio: Option<i32>, subtitle: Option<i32>) -> Self {
        Self { session, media, selection: Selection { audio, subtitle } }
    }
}
