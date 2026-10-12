
// backend/src/video.rs

//! 统一提供媒体探测, 播放会话和缓存管理接口, 隐藏具体实现子模块.
//!
//! 外部模块只从 `crate::video` 导入下列接口, 不依赖子模块路径.
//! 本文件通过重导出保留原类型及其方法, 不增加包装对象或改变调用逻辑.
//! HTTP 路由, 数据库查询和观看进度保存仍由调用方负责.
//!
//! 播放调用链:
//! `MetadataCache::get -> EncodeSpec::new -> SessionManager::create`.
//! 客户端后续通过会话查询状态, 更新目标, 读取媒体或释放租约.
//! 活动缓存去重, 分片生成与校验, 停止续接和磁盘淘汰均由内部实现处理.

mod assets;
mod metadata;
mod process;
mod ffmpeg;
mod fragment;
mod sessions;
mod cache;

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

/// 媒体探测结果, 可序列化为视频, 音频和字幕轨道信息.
///
/// 轨道选择使用容器内全局流索引. 时间轴原点和字体附件只供模块内部处理,
/// 不随 API 响应公开, 调用方无需自行解析 ffprobe 输出.
pub(crate) use metadata::MediaInfo;

/// 按源文件指纹复用探测结果的缓存.
///
/// `new(processes)` 使用共享媒体进程名额.
/// `get(id, path, retry)` 探测服务器文件地址, 同时缓存成功和失败结果.
/// `retry = true` 显式绕过已有结果, 探测错误仍向调用方返回.
pub(crate) use metadata::MetadataCache;

/// 服务端构造的单次播放执行参数.
///
/// `new(id, source, info, request, saved_position)` 校验指纹, 轨道和目标位置,
/// 合并历史进度与请求意图, 再交给 `SessionManager::create`.
/// 源文件地址, 目标位置和媒体处理细节不参与缓存身份计算.
pub(crate) use ffmpeg::EncodeSpec;

/// 稳定的缓存身份配置, 不等同于客户端播放请求.
///
/// `key()` 按字段声明顺序序列化, 保留可选轨道的 null, 再计算 MD5.
/// 配置只包含源文件指纹, 视频/音频/字幕流索引和编码契约.
/// 不应将文件路径, 会话 ID, 播放位置或动态线程数加入配置.
pub(crate) use cache::PlaybackVariantConfig;

/// 活动缓存及磁盘播放缓存的管理器.
///
/// `new(root, config, processes)` 检查现有缓存根目录并取得独占锁,
/// 初始化临时资源目录及执行磁盘容量维护.
/// `shutdown()` 拒绝新租约, 请求生成任务边界停止并等待所有写入者退出.
/// 获取缓存, 分片发布和淘汰接口只供模块内部的会话及生成任务使用.
pub(crate) use cache::CacheManager;

/// 媒体文件打开期间的缓存保护对象.
///
/// `SessionManager::media_path` 返回其 `Arc`, 调用方持有它直到文件打开完成,
/// 防止最后会话释放后 LRU 在文件打开之前删除缓存.
/// 外部不能直接创建, 查询或修改该对象, 媒体访问仍须经过有效会话.
pub(crate) use cache::ActiveCache;

/// 单个播放会话的状态快照, 可序列化为客户端响应.
///
/// complete 描述媒体完整性, generating 和 stopping 描述任务运行情况.
/// ready 表示已有可播放分片, 不保证整个目标位置已经被缓存覆盖.
/// 外部仅能读取校验目标位置所需的 duration, 其余字段通过序列化提供.
pub(crate) use sessions::SessionStatus;

/// 不透明播放会话及活动缓存租约的统一入口.
///
/// `new(cache, idle_seconds)` 关联共享缓存管理器和心跳超时.
/// `create(spec)` 预留会话, 获取共享缓存并等待恢复结果, 失败时清理租约.
/// `status(id)` 和 `heartbeat(id)` 查询或刷新存活时间.
/// `target(id, target)` 更新等待位置, 不保存数据库进度或重建缓存.
/// `media_path(id, name)` 校验租约和已发布资源, 返回地址, MIME 和保护对象.
/// `release(id)` 幂等释放租约, 不等待后台编码完成最后一个分片.
/// `spawn_expiration()` 启动过期清理, `shutdown()` 清空会话并关闭缓存管理器.
pub(crate) use sessions::SessionManager;

/// 跨模块测试用随机名称生成器, 不作为生产业务接口提供.
#[cfg(test)]
pub(crate) use cache::random_id;

/// 跨模块测试用媒体信息及请求样本, 不作为生产业务接口提供.
#[cfg(test)]
pub(crate) use ffmpeg::fixture;
