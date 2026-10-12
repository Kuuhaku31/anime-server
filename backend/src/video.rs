//! 统一提供媒体探测, 播放会话和缓存管理接口.
//!
//! 外部模块只从 `crate::video` 导入下列接口, 不依赖子模块路径.
//! 本文件通过重导出保留必要类型, 隐藏具体实现子模块.
//! 数据库偏好与观看进度仍由 API 层负责, 缓存和媒体路径解析由管理器负责.
//!
//! 播放调用链:
//! `SessionManager::create -> PlaybackVariantConfig::new -> CacheManager`.
//! 客户端后续通过会话查询状态, 更新目标, 读取媒体或释放租约.
//! 活动缓存去重, 分片生成与校验, 停止续接和磁盘淘汰均由内部实现处理.

mod assets;
mod cache;
mod ffmpeg;
mod fragment;
mod metadata;
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


/// 活动缓存及磁盘播放缓存的管理器.
///
/// `new(root, config, database, processes)` 检查现有缓存根目录并取得独占锁,
/// 初始化临时资源目录及执行磁盘容量维护.
/// `shutdown()` 拒绝新租约, 请求生成任务边界停止并等待所有写入者退出.
/// 获取缓存, 分片发布和淘汰接口只供模块内部的会话及生成任务使用.
pub(crate) use cache::{CacheManager, CachePin};

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
