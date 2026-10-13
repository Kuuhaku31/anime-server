# 后端核心模块与调用关系

后端由六个核心模块组成: API, Scanner, DB, SessionManager, CacheManager, Process.
`main.rs` 负责创建实例和管理生命周期, 不改变模块间的调用白名单.
`models`, `config`, `constants`, `utils` 是共享基础模块, 不属于核心模块,
不得借此形成被禁止的反向调用.

| 模块           | 源码                     | 一句话职责                                  |
| -------------- | ------------------------ | ------------------------------------------- |
| API            | `api.rs`, `api/`         | HTTP 路由, 请求解析和错误映射               |
| Scanner        | `scanner.rs`             | 只读遍历媒体目录并同步文件索引              |
| DB             | `db.rs`, `db/`           | SQLite 连接池, schema 和仓储操作            |
| SessionManager | `sessions.rs`            | 播放会话, 目标位置, 心跳和缓存租约          |
| CacheManager   | `cache.rs`, `cache/`     | 媒体元数据缓存, 活动播放缓存和磁盘 LRU      |
| Process        | `process.rs`, `process/` | FFmpeg/ffprobe 子进程, 资源预算和媒体流水线 |

## 调用白名单

每个模块只能直接调用表中列出的核心模块, 未列出的一律禁止.

| 调用方         | 允许直接调用                                       | 禁止直接调用                                        |
| -------------- | -------------------------------------------------- | --------------------------------------------------- |
| API            | Scanner, DB, SessionManager, CacheManager, Process | 无                                                  |
| Scanner        | DB                                                 | API, SessionManager, CacheManager, Process          |
| DB             | 无                                                 | API, Scanner, SessionManager, CacheManager, Process |
| SessionManager | CacheManager                                       | API, Scanner, DB, Process                           |
| CacheManager   | DB, Process                                        | API, Scanner, SessionManager                        |
| Process        | 无                                                 | API, Scanner, DB, SessionManager, CacheManager      |

```text
API --+--> Scanner ---------------------------> DB
      |                                        ^
      +--> DB                                  |
      |                                        |
      +--> SessionManager --> CacheManager ----+
      |                            |
      +--> CacheManager            `----------> Process
      |
      `--> Process
```

图中不存在的边均为禁止依赖. DB 和 Process 是依赖图的叶子,
不通过回调, trait object 或后台任务隐藏反向依赖.
API 虽然可以调用全部其他模块, 但只能使用其 `pub(crate)` 公开接口, 不得访问私有子模块或私有状态.

## 模块职责

### API

- 定义 HTTP 路由, 解析路径参数和 JSON 请求.
- 将数据库错误, 会话错误和媒体错误映射为 HTTP 状态及响应正文.
- 文件列表, 详情, 观看进度和识别信息直接调用 DB.
- 扫描接口调用 Scanner, 不自行遍历文件系统或写数据库.
- `/streams` 和 `/probe` 直接请求 CacheManager 的媒体信息.
- 播放创建, 状态, target, heartbeat, release 和媒体路径通过 SessionManager 协调,
  并显式传入 `&CacheManager`.
- 可读取 `ProcessManager::usage` 资源快照, 当前不提供对应 HTTP 路由.
- 不计算 variant-key, 不管理缓存状态, worker 或媒体子进程.

### Scanner

- 只读访问媒体目录, 识别候选视频文件并计算文件指纹.
- 调用 DB 查询已有路径, 新增或更新文件记录, 删除失效路径.
- 返回扫描变更摘要给 API 或后台扫描任务.
- 不探测轨道, 不生成 HLS, 不创建会话或缓存.

### DB

- 初始化 SQLite 连接池和 schema.
- 提供文件, 路径, 观看进度, 轨道偏好和识别字段的仓储操作.
- 为 CacheManager 提供 file ID 到媒体路径的查询, 为 Scanner 提供扫描同步事务.
- 只返回数据库模型和错误, 不读取媒体内容, 不启动进程, 不维护内存缓存.

### SessionManager

- 管理 session ID, variant-key 关联, 目标位置和心跳超时.
- 查询 CacheManager 的 MediaInfo, 构造 PlaybackVariantConfig 并计算播放目标.
- 调用 CacheManager 的 acquire, start, status, pause, media_path 和 release.
- 聚合同一 variant 的会话目标, 决定启动线程数; 最后一个租约结束时通知 CacheManager 释放缓存.
- 定时回收过期会话, 但不持有 CacheManager 字段.
- 不访问 DB 或 ProcessManager, 不接触 CacheFolder 和磁盘缓存细节.

### CacheManager

- 媒体元数据缓存, 活动播放缓存和磁盘 LRU 的统一入口.
- 通过 DB 将 file ID 解析为媒体路径, 通过 Process 执行 ffprobe, 编码和解码校验.
- 管理 CacheFolder Map, FolderPhase 状态机, CacheSnapshot 和 variant 目录.
- 使用自己的 JoinSet 管理 load/generate Tokio worker.
- 验证并原子发布 init, segment 和 playlist, 管理停止, 恢复, 完成, 失败和淘汰.
- 不管理 session ID, 心跳或客户端目标位置, 不构造 HTTP 响应.

### Process

- `ProcessManager` 管理 FFmpeg/ffprobe 进程名额, 编码器名额和编码线程预算.
- `ProcessManager::usage` 返回 `ProcessUsage` 只读快照, 包含 process slots,
  encoder slots 和 thread permits 的 total, used, available.
  快照不暴露 Semaphore, 命令, PID, 文件路径或取消发送端.
- 管理命令超时, 取消信号, stderr 尾部, 子进程 kill 和 wait.
- `process/ffmpeg.rs`: FFmpeg 命令构造, HLS 读写, 缓存媒体恢复和真实解码校验.
- `process/assets.rs`: 字幕和字体临时资源.
- `process/fragment.rs`: fMP4 结构校验和时间轴归一化.
- `process/models.rs`: EncodeSpec 等媒体执行参数.
- 接收路径, EncodeSpec, 取消信号和资源许可作为输入, 返回媒体结果或错误.
  不保存 CacheManager 引用或回调, 不持有 CacheManager 的 JoinSet.

## 关键调用链

### 创建播放会话

```text
API
-> DB 读取进度和轨道偏好
-> SessionManager::create(&CacheManager, file_id, request)
-> CacheManager::media_info(file_id)
   -> DB 查询媒体路径
   -> ProcessManager 执行 ffprobe
-> SessionManager 构造 PlaybackVariantConfig 和 session
-> CacheManager::acquire(config)
-> SessionManager 返回 SessionStatus 和 PlaybackStart
-> API 返回 JSON
```

### 生成播放缓存

```text
SessionManager::create/target
-> CacheManager::start
-> CacheManager JoinSet worker
-> ProcessManager 获取 encoder/process/thread 许可
-> process/ffmpeg.rs 启动 FFmpeg
-> process/fragment.rs 校验和归一化分片
-> CacheManager 原子发布文件并推进 FolderState
```

### 扫描媒体目录

```text
API 或后台定时任务
-> Scanner 遍历文件系统并计算指纹
-> DB 查询已有路径和大小
-> DB 事务更新 file 与 file_path
-> Scanner 返回 ScanChanges
```

### 关闭服务

```text
main
-> SessionManager::shutdown 清空会话
-> CacheManager::shutdown 标记停止并通知 load/generate worker
-> ProcessManager 取消或回收 FFmpeg/ffprobe 子进程
-> CacheManager 等待 JoinSet 清空并执行最终维护
```

## 依赖检查

- `api` 可引用 scanner, db, sessions, cache, process 的公开接口.
- `scanner` 只引用核心模块中的 db.
- `db` 不引用其他核心模块.
- `sessions` 只引用核心模块中的 cache.
- `cache` 只引用核心模块中的 db 和 process.
- `process` 不引用其他核心模块.
- 测试模块可以构造被测类型, 但生产代码不得通过测试辅助接口绕过白名单.
