//! 统一提供媒体执行, 分片校验和播放会话接口.
//!
//! 外部模块只从 `crate::video` 导入下列接口, 不依赖子模块路径.
//! 本文件通过重导出保留必要类型, 隐藏具体实现子模块.
//! 数据库偏好与观看进度仍由 API 层负责, 探测缓存和媒体路径解析由 crate::cache 负责.
//!
//! 播放调用链:
//! API 分别持有 SessionManager 和 CacheManager, 调用 create 时显式传入 CacheManager.
//! SessionManager 根据 file ID 和请求构造 PlaybackVariantConfig, 再委托缓存管理器获取状态.
//! 客户端后续通过会话查询状态, 更新目标, 读取媒体或释放租约.
//! crate::cache 管理活动缓存去重, 停止续接和磁盘淘汰, 本模块提供媒体生成与校验能力.

mod assets;
mod ffmpeg;
mod fragment;
mod process;
mod models;

/// 共享媒体资源限流器.
///
/// `new(total, thread_budget)` 创建媒体进程名额及编码线程预算.
/// 克隆实例共享同一组许可, 应由启动入口创建并交给探测和缓存管理器.
/// 它不是子进程注册表, 实际进程由执行器或缓存生成任务持有并回收.
pub(crate) use process::Processes;

/// 通用媒体命令执行器, 用于模块外的启动工具检查.
///
/// `checked_output(command, seconds)` 返回 stdout 字节, 负责超时,
/// stderr 诊断及子进程回收. 不自行获取 `Processes` 的并发许可.
pub(crate) use process::checked_output;


// 缓存模块通过这些媒体接口处理生成和校验, 不依赖私有子模块路径.
pub(crate) use assets::{clean_startup, RenderAssets};
pub(crate) use models::EncodeSpec;
pub(crate) use ffmpeg::{CachedMedia, command, has_video_after, recover, read_playlist, media_filename,
    regular_bytes, atomic_file, segment_name, sync_directory, write_playlist, validate_segment};
pub(crate) use fragment::{Init, Tail, normalize};
#[cfg(test)]
pub(crate) use ffmpeg::fixture;
pub(crate) use process::{cancelled, check_cancel, read_tail, Cancelled};

/// 不可变的播放变体身份配置, 由媒体信息和客户端轨道请求构造.
pub(crate) use models::playback_target;


/// 单个播放会话的状态快照, 可序列化为客户端响应.
///
/// phase 描述缓存文件夹生命周期, buffered_until 描述已验证媒体范围.
/// 外部仅能读取校验目标位置所需的 duration, 其余字段通过序列化提供.
pub(crate) use crate::sessions::SessionStatus;

/// 不透明播放会话及活动缓存租约的统一入口.
///
/// `new(idle_seconds)` 设置会话心跳超时, 不持有 CacheManager.
/// 缓存协调方法通过参数显式接收 CacheManager, create 仍以 file_id 和 request 为业务输入.
/// `spawn_expiration(cache_manager)` 启动过期清理, `shutdown()` 只清空会话.
pub(crate) use crate::sessions::SessionManager;
