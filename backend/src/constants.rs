
// backend/src/constants.rs
//! 定义容器目录和稳定媒体编码契约.

/// 扫描器允许的视频扩展名.
pub(crate) const EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
/// 内容身份使用的文件前缀字节数.
pub(crate) const HASH_LIMIT: u64 = 16 * 1024 * 1024;
/// SQLite 数据库的容器内固定位置.
pub(crate) const DATABASE_PATH: &str = "/data/main.db";
/// 持久数据根目录.
pub(crate) const DATA_DIR: &str = "/data";
/// 只读媒体根目录.
pub(crate) const VIDEO_ROOT: &str = "/media";
/// 番剧和剧集 API 缓存目录.
pub(crate) const DDP_ANI_CACHE_DIR: &str = "/data/ddp-ani";
/// 弹幕 API 缓存目录.
pub(crate) const DDP_DANMAKU_CACHE_DIR: &str = "/data/ddp-danmaku";
/// 用户通过 CACHE_DIR 独立挂载的容器内播放缓存根目录.
pub(crate) const CACHE_DIR: &str = "/cache";
/// 字幕渲染必需的 CJK 回退字体.
pub(crate) const FALLBACK_FONT: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
/// 编码或续接契约不兼容时升级此版本以隔离旧缓存.
pub(crate) const CACHE_VERSION: &str = "h264-high-yuv420p-aac-fmp4-v2";
