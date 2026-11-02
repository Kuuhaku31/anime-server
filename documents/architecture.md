# 核心架构

API 是业务枢纽, 使用按路径访问的媒体模型连接配置, 数据库, 扫描, 缓存和进程管理器.
SessionManager 是 API 内部的映射组件, 不保存播放状态.

## 实例与依赖

`services.rs` 使用六个 `OnceLock` 注册唯一实例, 不使用 `static mut`, unsafe 或运行时替换.
初始化顺序是 AppConfigManager, DatabaseManager, ProcessManager, CacheManager,
ScannerManager, SessionManager. Axum Router 不注入业务 AppState.

API, main 和后台扫描入口读取 services getter, 然后显式传入协作所需引用.
Manager 不调用 services getter, 不互相保存.
异步任务可克隆当前配置和 ProcessManager 的三组共享 Semaphore,
不会创建独立预算或将其他 Manager 写入结构体字段.

| 组件                | 持有的数据                                   | 职责与边界                            |
| ------------------- | -------------------------------------------- | ------------------------------------- |
| AppConfigManager    | 六个顶层配置字段                             | 配置读取, 默认值和合法性检查          |
| DatabaseManager     | 私有 SqlitePool                              | 唯一连接池, 按路径配置查询和扫描事务  |
| ScannerManager      | 媒体根目录, 扫描 Mutex                       | 路径边界, 前缀哈希, 全量和单路径扫描  |
| CacheManager        | root, handles                                | 管理全部实例目录与唯一句柄的对应关系  |
| CacheInstanceHandle | spec, handle_id, path, phase                 | 恢复, 启动, 停止, 状态转换和分片发布  |
| ProcessManager      | process_slots, encoder_slots, thread_permits | ffprobe/FFmpeg 命令, 预算, 超时和回收 |
| SessionManager      | Mutex&lt;HashMap&lt;String, PathBuf&gt;&gt;  | 不透明会话到缓存文件夹的映射          |

CacheManager 不探测媒体, 不访问数据库, 不直接运行媒体进程.
它把 source_path, metadata, 配置和进程引用交给句柄, 由句柄调用 ProcessManager.
API 只使用缓存管理器的快照和资源接口, 不访问句柄内部状态或计算 handle_id.

## 共享模型

前端媒体列表为 `[path, MediaInfo][]`. path 是容器内绝对路径,
同内容副本各自作为列表项, 前端不持有 media_id 或 handle_id.

| 模型           | 字段                                                                    |
| -------------- | ----------------------------------------------------------------------- |
| MediaInfo      | metadata, configs                                                       |
| MediaMetadata  | duration, format, video_streams, audio_streams, subtitle_streams        |
| StreamInfo     | stream_index, codec_name, language, title, is_default, is_forced        |
| MediaConfigs   | position, video_stream_index, audio_stream_index, subtitle_stream_index |
| PlaybackQuery  | path, video_stream_index, audio_stream_index, subtitle_stream_index     |
| HandleSnapshot | phase, buffered_until; 内部 folder 不序列化                             |
| PlaybackStart  | session_id, url, cache, media, 三类实际流索引                           |

MediaMetadata 内部额外保留容器起点, 字体附件和视频色彩信息,
供时间轴归一化, 字幕和 HDR 处理使用, 不作为 HTTP 字段公开.
文件名由前端从路径提取, 不再保存大小, 观看时间或识别字段.

## 数据库与扫描

`file` 以文件前 16 MiB 的 MD5 `media_id` 为主键, 保存共享 MediaConfigs.
`file_path` 以 `(media_id, path)` 为复合主键, 同时要求 path 唯一.
同一内容可以有多个路径, 同一路径只能关联一个当前内容身份.

扫描锁归 ScannerManager 所有. 文件遍历和前缀哈希在阻塞任务中执行,
全量扫描每次重算身份, 不依赖大小或修改时间.
全量结果在一个事务中应用, 建立全部新关联后再清理孤立配置,
因此文件移动且原路径被复用时不会丢失仍有副本的配置.

单路径扫描用于手动重扫和播放前重新计算身份. 文件消失时删除该路径,
最后一个副本消失时删除对应 file 行.
媒体列表由 API 逐路径实时调用 ProcessManager 探测, 不使用 metadata Map.

## 缓存身份与目录

CacheSpec 是 instance.json 和 handle_id 的唯一来源:

```json
{
  "media_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "video_stream_index": 0,
  "audio_stream_index": 1,
  "subtitle_stream_index": null,
  "version": "h264-high-yuv420p-aac-fmp4-v2"
}
```

Rust 按上述声明顺序序列化紧凑 JSON, 保留显式 null 后计算 MD5.
path, position, session_id, metadata 和线程数都不参与身份.
缓存配置通过 `PartialEq` 比较, 不再使用 CacheIdentity, CacheRequest, EncodeSpec 或 PlaybackSelection.

宿主机通过 `.env` 中的 CACHE_DIR 将播放缓存目录独立挂载到容器 `/cache`.
`/cache/playback/<handle_id>/` 必须与全量句柄表中的一个元素对应.
CacheManager 启动扫描合法目录, 校验 descriptor 和目录名一致后创建 Loading 句柄.
完成缓存恢复不要求源文件仍然存在, 部分缓存只在收到播放请求时续接.
已知哈希命名但身份无效的目录会清理, 无法安全清理或遇到未知名称则启动失败.
没有目录锁, 部署必须确保一个 CACHE_DIR 只供一个服务实例使用.

创建目录和持久化身份是短小且不可中途取消的临界区,
避免 HTTP 请求取消后留下有目录无句柄的状态.
除该创建临界区外, 媒体处理与文件 I/O 不持有句柄 phase 锁.

## 六状态生命周期

| 当前状态  | 允许的下一个状态                   |
| --------- | ---------------------------------- |
| Start     | Loading                            |
| Loading   | End, Completed, Ready, Failed      |
| Completed | End                                |
| Ready     | End, Running                       |
| Failed    | End, Loading                       |
| Running   | Completed, Ready, Failed, Stopping |
| Stopping  | End, Failed                        |

所有转换由 `transition` 校验. End 不是第七个阶段,
而是封闭句柄, 等写入者退出, 删除目录, 再移除 Map 项的动作.
phase 中的 None 仅用于封闭正在删除的句柄, 不可通过状态接口查询.

Loading 的 started 标记保证只启动一个恢复任务.
Ready 暂时保留恢复阶段的取消通道, 使停止操作能够回收尚未开始运行的续接请求.
Ready 的 run 在返回前登记 Running 和取消接收端,
不会在 shutdown 已返回后才启动编码.
多个播放请求等待恢复后竞争 Ready -> Running, 只有赢家启动编码,
其他请求共享同一输出. 已处于 Running 或 Completed 的打开操作幂等.
Failed 允许通过下一次 open 重新进入 Loading, 不自动无限重试.

Running 默认持续到 Completed, 不依赖 session 的数量或生命周期.
stop 将 Running 转为 Stopping, 发送取消并等待接收端全部退出,
任务失败则转为 Failed. 删除时再次检查写入状态, 防止排队的 run 在删除期间启动.
没有 workers 集合, Notify, 活动/磁盘双 Map, active/inactive, LRU 或全局 stopping 字段.

CachedMedia 只在 load/run 任务内存在. 状态接口从正式磁盘清单读取 buffered_until.
phase 锁不跨越编码, 解码或分片写盘.

## 媒体执行与发布

ProcessManager 为所有媒体子进程共享总名额, 编码器最多占用 `max_processes - 1`,
为分片校验保留至少一个位置. 编码线程使用固定 ffmpeg_threads,
受 max_total_ffmpeg_threads 约束, 不根据前端位置动态调节.

EncodingWorker 属于当前句柄任务, 负责精确进程的监控, stderr 尾部读取,
kill/wait 和许可归还. ProcessManager 不保存句柄引用, 回调或会话信息.

输出统一为 H.264 High, 8-bit yuv420p, 可选双声道 48 kHz AAC-LC 和 fMP4 HLS.
FFmpeg 只写私有 `.work-<random_id>/`. 字幕和字体也在该目录中准备,
任务成功, 失败或停止后统一清理.

句柄发布每个分片前比较 init 解码契约, 归一化逐轨时间轴和分片序号,
再配合正式 init 执行真实解码. 分片先落盘, 清单后原子替换,
已经发布的 init 和分片不被覆盖. 全部完成后提交 ENDLIST 和 complete 标记.
重启校验连续结构及首尾解码, 截断损坏尾部并保留可续接前缀.

## 播放与关闭流程

创建播放依次执行路径校验, 单文件重扫, 媒体探测, 读取共享配置,
API 直接选轨, 构造 CacheSpec, 打开缓存, 创建 session 映射并返回初始快照.
响应不等待媒体恢复或编码完成.

PlaybackController 仅在本地保存等待目标, 使用状态轮询确认
`position < buffered_until` 后挂载 HLS. 换轨保留原位置和播放意图,
未覆盖的 seek 继续使用同一 session, 不发出 target 或 heartbeat 请求.

DELETE 只删除 session 映射, 不停止或删除缓存.
未送达 DELETE 的映射保留到显式释放或服务重启, 不自动过期.
服务关闭先停止接收并等待 HTTP 请求结束, 再停止后台扫描和清空会话.
缓存逐句柄停止, 经 Stopping 的未完成实例执行 End,
已稳定的 Completed/Ready/Failed 实例保留以供重启恢复. 最后关闭数据库.
