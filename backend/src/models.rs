//! 定义数据库记录和接口数据结构.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{constants::HASH_LIMIT, utils};


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