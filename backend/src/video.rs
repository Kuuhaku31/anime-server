//! 统一提供媒体执行, 分片校验和播放会话接口.
//!
//! 外部模块只从 `crate::video` 导入下列接口, 不依赖子模块路径.
//! 本文件通过重导出保留必要类型, 隐藏具体实现子模块.
//! 数据库偏好与观看进度仍由 API 层负责, 探测缓存和媒体路径解析由 crate::cache 负责.
//!
//! 播放调用链:
//! `SessionManager::create -> PlaybackVariantConfig::new -> CacheManager`.
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
pub(crate) use process::{cancelled, check_cancel, read_tail, Cancelled};

/// 不可变的播放变体身份配置, 由媒体信息和客户端轨道请求构造.
pub(crate) use models::playback_target;


/// 单个播放会话的状态快照, 可序列化为客户端响应.
///
/// complete 描述媒体完整性, generating 和 stopping 描述任务运行情况.
/// ready 表示已有可播放分片, 不保证整个目标位置已经被缓存覆盖.
/// 外部仅能读取校验目标位置所需的 duration, 其余字段通过序列化提供.
pub(crate) use crate::sessions::SessionStatus;

/// 不透明播放会话及活动缓存租约的统一入口.
///
/// `new(cache, idle_seconds)` 关联缓存管理器并设置会话心跳超时.
/// `create(file_id, request)` 生成配置并获取共享缓存.
/// `status(id)` 和 `heartbeat(id)` 查询或刷新存活时间.
/// `target(id, target)` 更新等待位置, 不保存数据库进度或重建缓存.
/// `media_path(id, name)` 校验租约和已发布资源, 返回地址, MIME 和保护对象.
/// `release(id)` 幂等释放租约, 不等待后台编码完成最后一个分片.
/// `spawn_expiration()` 启动过期清理, `shutdown()` 清空会话并关闭缓存管理器.
pub(crate) use crate::sessions::SessionManager;
