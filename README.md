# Anime Server

本地视频 Web 服务, 使用单个 Docker 容器部署. Rust/Axum 提供媒体接口,
SQLite 保存播放配置, FFmpeg 生成可共享和恢复的 HLS 缓存,
TypeScript/HLS.js 提供常驻播放器和右侧文件列表.

## 挂载与端口

| 宿主机目录 | 容器目录   | 用途                                |
| ---------- | ---------- | ----------------------------------- |
| 配置目录   | `/config/` | 只读, 保存 `config.json`            |
| 视频目录   | `/media/`  | 只读, 保存媒体文件                  |
| 数据目录   | `/data/`   | 读写, 保存数据库和 API 缓存预留目录 |
| 缓存目录   | `/cache/`  | 读写, 保存播放缓存和任务临时文件    |

容器固定监听 `8080`, 宿主机通过根目录 `.env` 设置映射:

```dotenv
PORT=56080
MEDIA_DIR=/srv/media
CONFIG_DIR=/srv/anime-server/config
DATA_DIR=/srv/anime-server/data
CACHE_DIR=/srv/anime-server/cache
```

```text
/data/
|-- main.db
|-- ddp-ani/
`-- ddp-danmaku/

/cache/
`-- playback/
    `-- <handle_id>/
        |-- instance.json
        |-- index.m3u8
        |-- init.mp4
        |-- segment_000000.m4s
        |-- complete
        `-- .work-<random_id>/    # 仅在生成期间存在
```

`instance.json` 只包含媒体内容标识, 三类流索引和编码版本.
字幕与字体临时文件位于任务自己的 `.work-<random_id>/` 内, 任务结束时清理.
`ddp-ani/` 和 `ddp-danmaku/` 为 API 缓存预留目录, 当前没有番剧识别接口.
如需复用原数据目录内的播放缓存, 将 `CACHE_DIR` 指向宿主机 `${DATA_DIR}/cache` 对应的绝对路径.

## 代码与文档

```text
backend/src/
|-- main.rs, services.rs      # 启动与 OnceLock 全局实例
|-- config.rs                 # 扁平 AppConfigManager
|-- api.rs, api/              # HTTP 枢纽与内部 SessionManager
|-- db.rs, db/                # 唯一数据库连接池与事务仓储
|-- scanner.rs                # 媒体目录和单路径扫描
|-- cache.rs, cache/handle.rs # 全量句柄表与六状态生命周期
|-- process.rs, process/      # 探测, 子进程, 资源预算与 HLS 处理
|-- models.rs, models/        # MediaInfo, PlaybackQuery 与 CacheSpec
`-- tests/                    # 集中 Rust 测试和夹具
frontend/src/
|-- api/                      # 同源 HTTP 客户端
|-- state/                    # 共享数据契约
|-- playback/                 # 本地等待目标与 HLS 引擎
`-- ui/                       # 控件和按路径展示的媒体列表
```

- [核心架构](documents/architecture.md)
- [HTTP API](documents/api.md)
- [数据库](documents/database.md)
- [开发, 部署与验证](documents/development.md)

## 前端 (浏览器)

前端持有的核心数据

- `session_id`: 和后端会话对应的唯一标识
- 前端媒体列表: `List<(Path, MediaInfo)>`: 媒体文件在后端容器内的路径及其对应的信息

MediaInfo 包含

- `metadata`: 媒体文件的元数据信息
  - `duration`: 媒体文件的时长
  - `format`: 媒体文件的格式
  - `video_streams`: 视频流信息列表
  - `audio_streams`: 音频流信息列表
  - `subtitle_streams`: 字幕流信息列表
- `configs`: 媒体文件的配置信息
  - `position`: 上次播放位置
  - `video_string_index`: 上次播放的视频流索引
  - `audio_string_index`: 上次播放的音频流索引
  - `subtitle_string_index`: 上次播放的字幕流索引

## 后端 (服务器)

包括以下主要模块:

- AppConfigManager
- API接口枢纽
- 会话管理器
  - `HashMap<SessionId, PathBuf>`
- 缓存管理器
  - 持有 缓存实例句柄列表: `List<CacheInstanceHandle>`
- 进程管理器 直接操作ffmpeg进行编解码
- 数据库管理器 提供数据库操作
- 扫描管理器 周期性扫描媒体文件更新数据库

## 数据库

- 文件表
  媒体标识: 媒体文件前16MiB MD5哈希值为主键,
  对应的文件各项配置

- 文件路径表
  MD5哈希值和文件在容器内的路径为主键

## 主要业务流程

1. 前端 初始化/刷新 前端媒体列表

- 前端 向后端发送前端媒体列表请求
- 后端 收到请求, 查询 数据库文件路径表 获得所有文件路径及其对应的 MD5 哈希值
- 后端 根据 MD5 哈希值查询文件表 构建 `List<(Path, configs)>`
- 后端 根据文件路径*调用工具访问文件获得对应 metadata*, 构建 `List<(Path, MediaInfo)>`
- 后端 向前端发送 `List<(Path, MediaInfo)>`

2. 前端 播放视频

- 前端 根据 前端媒体列表 选择要播放的媒体文件和需要的播放配置
- 前端 构建并向后端发送 `PlaybackQuery`
- 后端 API接口枢纽 收到 `PlaybackQuery` 后, 根据 `path` 访问媒体文件获得:
  - 新计算的 媒体标识
  - 获取其元数据信息 `metadata` (包括时长, 格式, 视频流, 音频流, 字幕流...)
- API接口枢纽 根据 PlaybackQuery 中的所期望的播放配置和 metadata 实际值进行比对
- API接口枢纽 综合生成`(媒体地址, 实际播放的视频/音频/字幕流索引)`, 并传递给 缓存管理器
  - 缓存管理器 根据这些参数计算出句柄标识后,
    查找 缓存实例句柄列表, 找到或创建对应的 缓存实例句柄.
    缓存实例句柄列表的每个句柄必须和实际缓存实例文件夹一一对应
  - 缓存实例句柄的状态不是 Complete 时, 缓存管理器启动相应的转码任务
- API接口枢纽 从 缓存管理器 获取缓存实例的路径, 构建播放流, 返回给前端
- 前端 根据返回的播放流进行播放
- 如果前端播放的位置还没有生成, 需要等待缓存实例完成转码后再开始播放

### 前端的播放请求 `PlaybackQuery`

包含

- 媒体文件在后端容器内的路径: `path`
- 请求的视频流索引: `video_stream_index`
- 请求的音频流索引: `audio_stream_index`
- 请求的字幕流索引: `subtitle_stream_index`

### 缓存实例句柄: `CacheInstanceHandle`

缓存管理器初始化时先扫描缓存文件夹, 为每个缓存实例文件夹创建对应的缓存实例句柄
缓存实例句柄 调用相关编码工具 操作对应的缓存实例文件夹
包含

- 句柄标识: `handle_id` 根据字段(媒体标识, 实际转码用到的视频/音频/字幕流索引)计算hash获得
- 媒体标识: `media_id`
- 实际转码用到的视频流索引: `video_stream_index`
- 实际转码用到的音频流索引: `audio_stream_index`
- 实际转码用到的字幕流索引: `subtitle_stream_index`
- 版本标识: `version`
- 容器内保存对应的缓存实例文件夹路径: `path`

缓存实例句柄有且只有以下6个状态

- 加载中: `Loading`
- 已完成: `Completed`
- 待启动: `Ready`
- 异常: `Failed`
- 正在停止: `Stopping`
- 正在转码: `Running`

状态转移矩阵

| 当前状态  | 允许的下一个状态                   |
| --------- | ---------------------------------- |
| 开始      | Loading                            |
| Loading   | 结束, Completed, Ready, Failed     |
| Completed | 结束                               |
| Ready     | 结束, Running                      |
| Failed    | 结束, Loading                      |
| Running   | Completed, Ready, Failed, Stopping |
| Stopping  | 结束, Failed                       |

#

> 2026-11-02T14:18:37+09:00
