//! 定义应用程序中使用的常量和配置参数.

use std::path::Path;

pub(crate) const EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
pub(crate) const HASH_LIMIT: u64 = 16 * 1024 * 1024;


// 容器路径保持固定, 宿主机目录和端口映射由 Compose 与 .env 决定.
pub(crate) const DATABASE_PATH: &str = "/data/main.db";
pub(crate) const DATA_DIR: &str = "/data";
pub(crate) const VIDEO_ROOT: &str = "/media";
pub(crate) const DDP_ANI_CACHE_DIR: &str = "/data/ddp-ani";
pub(crate) const DDP_DANMAKU_CACHE_DIR: &str = "/data/ddp-danmaku";
pub(crate) const CACHE_DIR: &str = "/cache";
pub(crate) const FALLBACK_FONT: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";


pub(crate) fn video_root_path() -> &'static Path { Path::new(VIDEO_ROOT) }


// 编码或续接规则改变兼容性时必须升级, 避免新任务复用旧契约的缓存.
pub(crate) const ENCODER_PROFILE: &str = "h264-high-yuv420p-aac-fmp4-v2";