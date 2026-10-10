
// backend/src/constants.rs
//! 定义应用程序中使用的常量和配置参数.

pub const EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
pub const HASH_LIMIT: u64 = 16 * 1024 * 1024;


// 容器路径保持固定, 宿主机目录和端口映射由 Compose 与 .env 决定.
pub const DATABASE_PATH: &str = "/data/main.db";
pub const DATA_DIR: &str = "/data";
pub const VIDEO_ROOT: &str = "/media";
pub const DDP_ANI_CACHE_DIR: &str = "/data/ddp-ani";
pub const DDP_DANMAKU_CACHE_DIR: &str = "/data/ddp-danmaku";
pub const CACHE_DIR: &str = "/cache";
pub const FALLBACK_FONT: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
