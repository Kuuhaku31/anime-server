# 核心架构

API 是业务枢纽, 使用按路径访问的媒体模型连接配置, 数据库, 扫描, 缓存和进程管理器.
播放通过稳定 handle_id 直接访问缓存资源, 没有 SessionManager 或服务端播放映射.

## 实例与依赖

`services.rs` 使用五个 `OnceLock` 注册唯一实例, 不使用 `static mut`, unsafe 或运行时替换.
初始化顺序是 AppConfigManager, DatabaseManager, ProcessManager, CacheManager,
ScannerManager. Axum Router 不注入业务 AppState.

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
| CacheInstanceHandle | spec, handle_id, path, phase, resources      | 恢复, 启停, 状态转换, 分片发布和 worker 占用 |
| ProcessManager      | process_slots, encoder_slots, thread_permits | ffprobe/FFmpeg 命令, 预算, 超时和回收 |

CacheManager 不探测媒体, 不访问数据库, 不直接运行媒体进程.
它把 source_path, metadata, 配置和进程引用交给句柄, 由句柄调用 ProcessManager.
API 只使用缓存管理器的快照和资源接口, 不访问句柄内部状态或计算 handle_id.
CacheManager 的查询和资源接口只接收 handle_id, 不接收客户端提供的缓存目录.
列表复制句柄后释放全局 Map 锁, 再按 handle_id 排序读取快照.

## 共享模型

前端媒体列表为 `[path, MediaInfo][]`. path 是容器内绝对路径,
同内容副本各自作为列表项. 另有 `Map<handle_id, CacheEntry>` 保存全部缓存的内存快照,
页面刷新后由服务端重建, 不持久化到浏览器存储.

| 模型           | 字段                                                                    |
| -------------- | ----------------------------------------------------------------------- |
| MediaInfo      | metadata, configs                                                       |
| MediaMetadata  | duration, format, video_streams, audio_streams, subtitle_streams        |
| StreamInfo     | stream_index, codec_name, language, title, is_default, is_forced        |
| MediaConfigs   | position, video_stream_index, audio_stream_index, subtitle_stream_index |
| PlaybackQuery  | path, video_stream_index, audio_stream_index, subtitle_stream_index     |
| CacheStatus    | phase, buffered_until, error                                            |
| CacheSnapshot  | handle_id, spec, status, resources, source_path                        |
| CacheBuild     | cache, media; 实际流选择位于 cache.spec                                  |
| CacheList      | entries                                                               |
| WorkerResources | encoder_slots, process_slots, ffmpeg_threads                           |

MediaMetadata 内部额外保留容器起点, 字体附件和视频色彩信息,
供时间轴归一化, 字幕和 HDR 处理使用, 不作为 HTTP 字段公开.
文件名由前端从路径提取, 不再保存大小, 观看时间或识别字段.

## 数据库与扫描

`file` 以文件前 16 MiB 的 MD5 `media_id` 为主键, 保存共享 MediaConfigs.
`file_path` 以 `(media_id, path)` 为复合主键, 同时要求 path 唯一.
同一内容可以有多个路径, 同一路径只能关联一个当前内容身份.

扫描锁归 ScannerManager 所有. 文件遍历和前缀哈希在阻塞任务中执行,
全量扫描每次重算身份, 不依赖大小或修改时间.
全量结果在一个事务中应用, 文件移动且原路径被复用时不会丢失原内容的配置.

单路径扫描用于手动重扫和播放前重新计算身份. 文件消失时删除该路径,
当前数据库保留没有路径的 file 配置, 不因删除缓存而删除观看配置.
媒体列表由 API 逐路径实时调用 ProcessManager 探测, 不使用 metadata Map.
缓存列表按 media_id 查询排序后的路径, 跳过已经消失的副本,
不在每次轮询中运行 ffprobe. 重试必须重扫源路径并比对原 media_id,
内容已被替换则明确拒绝, 不把旧句柄续接到新媒体.

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
path, position, metadata 和线程数都不参与身份.
缓存配置通过 `PartialEq` 比较, 不再使用 CacheIdentity, CacheRequest, EncodeSpec 或 PlaybackSelection.

宿主机通过 `.env` 中的 CACHE_DIR 将播放缓存目录独立挂载到容器 `/cache`.
`/cache/playback/<handle_id>/` 必须与全量句柄表中的一个元素对应.
CacheManager 启动扫描合法目录, 校验 descriptor 和目录名一致后创建 Loading 句柄.
完成缓存恢复不要求源文件仍然存在, 部分缓存只在收到构建或重试请求时续接.
无源缓存仍保留和显示, 但按接口策略禁止播放和重试, 只能管理或删除.
身份无效或名称未知的目录保留原文件并阻止启动, 由用户修复或手动移除, 不自动删除缓存实例.
没有目录锁, 部署必须确保一个 CACHE_DIR 只供一个服务实例使用.

创建目录和持久化身份是短小且不可中途取消的临界区,
避免 HTTP 请求取消后留下有目录无句柄的状态.
编码, 解码和分片发布不持有句柄 phase 锁. 快照和停止完成确认只在锁内读取短小的正式清单.

## 六状态生命周期

| 当前状态  | 允许的下一个状态                   |
| --------- | ---------------------------------- |
| Start     | Loading                            |
| Loading   | End, Completed, Ready, Failed      |
| Completed | End                                |
| Ready     | End, Running                       |
| Failed    | End, Loading                       |
| Running   | Completed, Ready, Failed, Stopping |
| Stopping  | End, Failed, Ready, Completed      |

所有转换由 `transition` 校验. End 不是第七个阶段,
而是封闭句柄, 等写入者退出, 删除目录, 再移除 Map 项的动作.
phase 中的 None 仅用于封闭正在删除的句柄, 不可通过状态接口查询.

Loading 的 started 标记保证只启动一个恢复任务.
Ready 暂时保留恢复阶段的取消通道, 使停止操作能够回收尚未开始运行的续接请求.
Ready 的 run 在返回前登记 Running 和取消接收端,
不会在 shutdown 已返回后才启动编码.
多个构建请求等待恢复后竞争 Ready -> Running, 只有赢家启动编码,
其他请求共享同一输出. 已处于 Running 或 Completed 的构建操作幂等.
Failed 允许通过下一次 build 或 retry 重新进入 Loading, 不自动无限重试.

Running 默认持续到 Completed, 不依赖浏览器数量或是否仍在播放.
stop 将 Running 转为 Stopping, 发送取消并等待接收端全部退出,
任务失败则转为 Failed. 用户停止后转为 Ready, 完成清单已提交时转为 Completed,
这两个转换必须确认全部写入者退出. 已发布前缀在停止期间仍可读取.
删除时再次检查写入状态, 防止排队的 run 在删除期间启动.
没有 workers 集合, Notify, 活动/磁盘双 Map, active/inactive, LRU 或全局 stopping 字段.

CachedMedia 只在 load/run 任务内存在. 状态接口从正式磁盘清单读取 buffered_until.
phase 锁不跨越编码, 解码或分片写盘.

## 媒体执行与发布

ProcessManager 为所有媒体子进程共享总名额, 编码器最多占用 `max_processes - 1`,
为分片校验保留至少一个位置. 编码线程使用固定 ffmpeg_threads,
受 max_total_ffmpeg_threads 约束, 不根据前端位置动态调节.

EncodingWorker 属于当前句柄任务, 负责精确进程的监控, stderr 尾部读取,
kill/wait 和许可归还. ProcessManager 不保存句柄引用, 回调或会话信息.

每个句柄持有一个 WorkerUsage 原子计数, 只在编码进程启动后发布线程预算,
由同一个值推导 encoder_slots 和 process_slots. WorkerBudget 与实际许可共同释放时清零.
正常完成, 启动失败, 取消和停止都不会留下虚假占用.
统计不包括短时探测, 字幕准备和分片校验, 也不是 CPU 或浏览器解码指标.

输出统一为 H.264 High, 8-bit yuv420p, 可选双声道 48 kHz AAC-LC 和 fMP4 HLS.
FFmpeg 只写私有 `.work-<random_id>/`. 字幕和字体也在该目录中准备,
任务成功, 失败或停止后统一清理.

句柄发布每个分片前比较 init 解码契约, 归一化逐轨时间轴和分片序号,
再配合正式 init 执行真实解码. 分片先落盘, 清单后原子替换,
已经发布的 init 和分片不被覆盖. 全部完成后提交 ENDLIST 和 complete 标记.
重启校验连续结构及首尾解码, 截断损坏尾部并保留可续接前缀.

## 播放与关闭流程

构建依次执行路径校验, 单文件重扫, 媒体探测, 读取共享配置,
API 直接选轨, 构造 CacheSpec, 构建或复用缓存并返回 CacheBuild.
响应不等待媒体恢复或编码完成.

PlaybackController 仅在本地保存等待目标, 使用状态轮询确认
`position < buffered_until` 后挂载 HLS. 换轨保留原位置和播放意图,
未覆盖的 seek 继续使用同一 handle, 不重新构建或向服务端发送等待目标.
CacheStore 统一更新全部缓存, 播放器和缓存列表订阅同一份数据.
文件列表的 Build 不改变播放器. 缓存列表的 Play 只校验源并读取已有资源.

播放器停止, 解码失败或页面关闭仅卸载本地 HLS.
缓存 Stop 保留前缀, DELETE 立即停止并删除目录. 后续资源请求返回 404,
当前播放器在下一次列表刷新或资源失败时立即卸载, 不自动重建.
stop 和 delete 在独立任务中完成, 不因 HTTP 客户端断开而中途取消清理.
服务关闭先停止接收并等待 HTTP 请求结束, 再停止后台扫描.
缓存逐句柄停止并回收写入任务, 所有实例及已发布前缀均持久保留, 不执行 End.
重启后恢复完整缓存或有效前缀, 未完成实例等待显式构建或续接. 最后关闭数据库.
