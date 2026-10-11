//! 定义数据库记录和接口数据结构.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{constants::HASH_LIMIT, utils};


#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct VideoFile {
    pub pre16mib_md5: String,
    pub(crate) name: String,
    pub(crate) size: i64,
    pub(crate) audio_stream_index: Option<i32>,
    pub(crate) subtitle_stream_index: Option<i32>,
    pub(crate) ddp_epi_id: Option<i64>,
    pub(crate) danmaku_offset: f64,
    pub(crate) watch_pos: Option<f64>,
    pub(crate) watched_datetime: Option<String>,
    pub(crate) path: String,
    pub(crate) name_no_ext: String,
    pub(crate) extension: String,
}


#[derive(Debug, Deserialize)]
pub(crate) struct ProgressInput {
    pub(crate) position: f64,
}

/// 进度按文件指纹保存原视频绝对秒数, 跨路径副本和不同播放配置复用.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct WatchProgress {
    pub(crate) pre16mib_md5: String,
    pub(crate) watch_pos: Option<f64>,
    pub(crate) watched_datetime: Option<String>,
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
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
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
    pub(crate) fn file_dir          (&self) -> Option<&std::path::Path> { self.path.parent() }
    pub(crate) fn file_name_no_ext  (&self) -> Option<&str> { self.path.file_stem().and_then(|s| s.to_str()) }
    pub(crate) fn file_extension    (&self) -> Option<&str> { self.path.extension().and_then(|s| s.to_str()) }
}


pub(crate) struct ScanChanges {
    pub(crate) added: Vec<ScannedFile>,
    pub(crate) removed: Vec<PathBuf>,
    pub(crate) count: u64, // 所有现存视频路径数
}