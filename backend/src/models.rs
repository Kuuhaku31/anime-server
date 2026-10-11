//! 定义数据库记录和接口数据结构.

use serde::{Deserialize, Serialize};

/// 文件列表的去重记录, 同一指纹存在多个副本时只选一条路径作为展示信息.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct VideoFile {
    pub pre16mib_md5: String,
    pub name: String,
    pub size: i64,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    pub ddp_epi_id: Option<i64>,
    pub danmaku_offset: f64,
    pub watch_pos: Option<f64>,
    pub watched_datetime: Option<String>,
    pub path: String,
    pub name_no_ext: String,
    pub extension: String,
}

/// 相对于媒体根目录的路径拆分记录, 同一文件指纹可以对应多个副本.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FilePath {
    pub path: String,
    pub name_no_ext: String,
    pub extension: String,
    pub pre16mib_md5: String,
}

impl FilePath {
    pub fn relative_path(&self) -> String {
        let name = format!("{}.{}", self.name_no_ext, self.extension);
        if self.path.is_empty() {
            name
        } else {
            format!("{}/{}", self.path, name)
        }
    }
}

#[derive(Debug)]
pub struct ScannedFile {
    pub pre16mib_md5: String,
    pub size: i64,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    pub path: String,
    pub name_no_ext: String,
    pub extension: String,
}

#[derive(Debug, Deserialize)]
pub struct ProgressInput {
    pub position: f64,
}

/// 进度按文件指纹保存原视频绝对秒数, 跨路径副本和不同播放配置复用.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct WatchProgress {
    pub pre16mib_md5: String,
    pub watch_pos: Option<f64>,
    pub watched_datetime: Option<String>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
}
