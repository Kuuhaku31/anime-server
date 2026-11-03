# Anime Server

本地视频 Web 服务, 使用单个 Docker 容器部署. Rust/Axum 提供媒体接口,
SQLite 保存播放配置和 DandanPlay 剧集关联, FFmpeg 生成可共享和恢复的 HLS 缓存,
TypeScript/HLS.js 提供常驻播放器, 时间同步弹幕列表和右侧文件及缓存任务列表.

## 挂载与端口

| 宿主机目录 | 容器目录   | 用途                                |
| ---------- | ---------- | ----------------------------------- |
| 配置目录   | `/config/` | 只读, 保存 `config.json`            |
| 视频目录   | `/media/`  | 只读, 保存媒体文件                  |
| 数据目录   | `/data/`   | 读写, 保存数据库和弹幕 API 缓存     |
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
    `-- <episode_id>.json    # DandanPlay 原始弹幕 JSON

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
`ddp-ani/` 仍为番剧 API 缓存预留目录, `ddp-danmaku/` 按剧集保存原始弹幕 JSON.
文件识别的 episode ID 保存到 main.db, 弹幕内容不进入数据库.
弹幕仅在播放器下方以时间同步列表显示, 不增加视频画面弹幕层.
如需复用原数据目录内的播放缓存, 将 `CACHE_DIR` 指向宿主机 `${DATA_DIR}/cache` 对应的绝对路径.

## 代码与文档

```text
backend/src/
|-- main.rs, services.rs      # 启动与 OnceLock 全局实例
|-- config.rs                 # JSON 运行配置与环境认证参数
|-- api.rs                    # 媒体接口, 剧集关联, 弹幕与缓存资源
|-- dandanplay.rs             # 开放 API 签名, 文件识别与弹幕下载
|-- danmaku.rs                # 原始弹幕文件缓存与规范化响应
|-- db.rs                     # 唯一数据库连接池与事务仓储
|-- scanner.rs                # 媒体目录和单路径扫描
|-- cache.rs, cache/handle.rs # 全量句柄表与六状态生命周期
|-- process.rs, process/      # 探测, 子进程, 资源预算与 HLS 处理
`-- models.rs                # 媒体模型, PlaybackQuery 与 CacheSpec
backend/tests/               # 集中 Rust 测试和夹具
frontend/src/
|-- api/                      # 同源 HTTP 客户端
|-- state/                    # 共享数据契约和 handle_id 缓存 Map
|-- playback/                 # 本地等待目标与 HLS 引擎
`-- ui/                       # 控件, 媒体列表, 缓存任务和弹幕列表
```

- [核心架构](documents/architecture.md)
- [HTTP API](documents/api.md)
- [数据库](documents/database.md)
- [开发, 部署与验证](documents/development.md)

## 前端 (浏览器)

前端持有的核心数据

- `handle_id`: 服务端计算的稳定缓存标识, 不作为访问凭证
- `Map<handle_id, CacheEntry>`: 内存中保存全部缓存的状态, 生成进度, 实际流选择和 worker 资源
- 前端媒体列表: `MediaEntry[]`, 包含 file_id, path, configs 和可空 episode_id

页面刷新后从 `GET /api/cache` 重建 Map, 不把任务快照保存在 localStorage.
文件列表的 Build 只提交转码且不切换播放器, 剧集关联也不触发播放或构建.
缓存列表的 Play 只读取已有缓存, Retry / resume 重新构建, Stop build 保留已发布前缀,
Delete 经确认后立即停止并删除缓存, 包括其他浏览器正在使用的缓存.
播放器的 Stop 只停止本地播放, 不发送服务端删除或停止转码请求.

每条缓存显示 `encoder_slots`, `process_slots`, `ffmpeg_threads`, 并显示全部条目的合计.
这些数字是已启动转码 worker 的预算占用, 不是 CPU 使用率, 操作系统实际线程数或浏览器解码开销.
排队和准备阶段可为 running 且三项占用均为 0.

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
- 缓存管理器
  - 持有 `HashMap<handle_id, Arc<CacheInstanceHandle>>`
- 进程管理器 直接操作ffmpeg进行编解码
- 数据库管理器 提供数据库操作
- 扫描管理器 周期性扫描媒体文件更新数据库

## 数据库

- 文件表
  媒体标识: 媒体文件前16MiB MD5哈希值为主键,
  对应的文件各项播放配置和可空 DandanPlay episode ID

- 文件路径表
  MD5哈希值和文件在容器内的路径为主键

## 主要业务流程

1. 前端 初始化/刷新 前端媒体列表

- 前端 向后端发送前端媒体列表请求
- 后端 收到请求, 查询 数据库文件路径表 获得所有文件路径及其对应的 MD5 哈希值
- 后端 根据 MD5 哈希值查询文件表, 返回 file_id, path, configs 和 episode_id
- 列表不读取媒体内容或运行 ffprobe, metadata 由前端按 file_id 单独按需请求

2. 前端 构建缓存

- 前端 根据 前端媒体列表 选择要播放的媒体文件和需要的播放配置
- 前端 向 `POST /api/cache` 发送 `PlaybackQuery`
- 后端 API接口枢纽 收到 `PlaybackQuery` 后, 根据 `path` 访问媒体文件获得:
  - 新计算的 媒体标识
  - 获取其元数据信息 `metadata` (包括时长, 格式, 视频流, 音频流, 字幕流...)
- API接口枢纽 根据 PlaybackQuery 中的所期望的播放配置和 metadata 实际值进行比对
- API接口枢纽 综合生成`(媒体地址, 实际播放的视频/音频/字幕流索引)`, 并传递给 缓存管理器
  - 缓存管理器 根据这些参数计算出句柄标识后,
    查找 缓存实例句柄列表, 找到或创建对应的 缓存实例句柄.
    缓存实例句柄列表的每个句柄必须和实际缓存实例文件夹一一对应
  - running 或 completed 复用已有任务, ready 启动生成, failed 重新恢复后重试
- API 返回 `CacheBuild { cache, media }`, 不返回内部目录或创建播放映射
- `GET /api/cache` 和 `GET /api/cache/{handle_id}` 只查询状态, 不触发构建

3. 前端 播放已有缓存

- 前端按 `handle_id` 选择缓存, 确认其现存源路径可用
- 浏览器通过 `GET /playback/{handle_id}/{asset_name}` 读取 HLS 清单和分片
- 构建和播放是不同请求. 只有 `position < buffered_until` 时才加载对应位置
- retry 按 `media_id` 选择稳定排序后的首个现存源路径, 重扫身份并重新验证流选择
- 没有源路径的缓存仍显示且可以删除, 但不允许播放或重试
- 不存在 SessionManager, session_id, 会话释放或心跳

4. 弹幕识别

- 数据库文件表保存 Dandanplay episode ID 与媒体标识的对应关系
- 前端 媒体列表展示 Dandanplay episode ID 的 UI 控件:
  - ID 显示/编辑框: 用户可编辑并写回数据库
  - 识别按钮: 可重复点击并访问 DandanPlay API, 无需先清空已有 ID.
    唯一精确结果覆盖原 ID, 无唯一结果时保留原 ID, 具体流程如下:

- 用户点击识别按钮
- 前端 把媒体文件地址发送给后端
- 后端 API枢纽 根据地址获得该文件前16MiB 的 MD5 哈希值, 文件大小, 文件名
- 后端 API枢纽 根据这些参数访问Dandanplay API 进行文件识别
- 后端 API枢纽 将唯一识别结果写回数据库, 返回最终 ID 和本次是否匹配

实际接口为 `PUT /api/media/{file_id}/episode` 和 `POST /api/media/identify`.
识别请求使用 JSON 路径字符串, 与单文件重扫一致.
`match_file` 返回 `Result<Option<u64>>`, 只在上游标记精确且结果唯一有效时返回 ID.
唯一结果覆盖已有或手动保存的 ID, 模糊结果或没有匹配返回 None, 不再提供候选选择.
无唯一匹配和上游请求失败都保留原关联, 分别显示结果或错误, 不随意采用第一项.
识别 API 返回最终 episode_id 和 matched 标志, 可区分新匹配与保留的旧 ID.
输入框清空后点击 Save ID 会保存 NULL, 同内容副本共享 ID, 播放进度保存不会覆盖它.
现有 media_id 数据库启动时自动增补 episode_id 字段, 保留已有记录.

识别认证从环境变量获取, 在项目根目录 `.env` 中设置自己获授权的参数:

```dotenv
DANDAN_APP_ID=YOUR_APP_ID
DANDAN_APP_SECRET=YOUR_APP_SECRET
```

Docker Compose 将这两项传入容器环境, 后端启动时读取, 不直接加载 `.env` 文件.
修改 `.env` 后执行 `docker compose up -d --build` 重新创建服务, 仅 restart 不会更新容器环境.
直接运行后端时需向进程导出这两个环境变量.
`config.json` 不再接受 dandanplay 字段, 升级时请移除旧认证配置.
两项均未设置或为空时不影响本地播放和手动编辑, 识别请求会明确返回未配置错误.
仅设置一项或提供无效值时启动报错, 不回退到 JSON 中的凭据.
凭据仅由后端读取, 浏览器不接收密钥; 上游只收到文件名, 前 16 MiB MD5 和文件大小,
不会收到媒体完整路径或视频内容.
签名和匹配协议参考 [NipaPlay-Reload](https://github.com/AimesSoft/NipaPlay-Reload)
与 [anime-tool](https://github.com/Kuuhaku31/anime-tool), 不复用其应用凭据.
识别和弹幕加载相互独立, 保存或识别 ID 不自动下载弹幕.

5. 弹幕加载

播放器下方显示当前媒体的 DandanPlay episode ID, Load comments 和 Refresh comments,
以及独立滚动的弹幕列表. 切换媒体或修改其 ID 会清空旧列表并取消旧请求, 不自动加载.

缓存结构:

```text
/data/
`-- ddp-danmaku/
    `-- <episode_id>.json
```

前端向 `POST /api/danmaku` 发送:

```json
{ "episode_id": 1234567, "refresh": false }
```

Load comments 优先读取已有缓存, 缺失时访问 DandanPlay; Refresh comments 发送
`refresh:true`, 始终跳过缓存检查. 同剧集请求串行化, 并发缓存未命中不会重复下载.
上游使用 `GET /api/v2/comment/{episode_id}?withRelated=true&chConvert=0`,
复用环境变量中的应用凭据, 包含关联弹幕且不转换简繁体.
缓存命中不需要凭据, 需要下载但未配置凭据时明确返回 `503`.

后端校验新结果后原子保存原始 JSON, 直接返回按时间稳定排序的弹幕 JSON 数组,
不包含 `episode_id`, `comments` 或 `cached` 包装字段, 空列表返回 `[]`.
每条包含 time, content, mode, color, sender 和字符串 comment_id.
缓存损坏时明确报错, 可用 Refresh comments 修复, 不静默返回空列表.
上游失败不替换旧缓存, 前端刷新失败也保留已显示列表并显示错误.
弹幕缓存不自动过期或清理, 单份原始 JSON 上限为 32 MiB.

表项以左侧完整内容为主, 右侧显示时间, 模式, 颜色, sender 和 cid 位于次要信息行.
列表高亮并滚动到最近一条 `time <= 当前播放位置` 的弹幕,
同时间定位最后一条, seek 前后跳转立即重新定位, 首条之前取消高亮并回到顶部.
原生全屏和页内全屏隐藏弹幕列表, 退出后恢复当前位置.
本轮不实现视频画面弹幕层, 过滤, 发送或用户登录.
完整请求, 响应和错误语义见 [HTTP API](documents/api.md#弹幕加载).

### 前端的构建请求 `PlaybackQuery`

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
- 长时间转码 worker 的原子资源计数: `resources`

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
| Stopping  | 结束, Failed, Ready, Completed     |

显式 Stop build 等待写入者退出后转为 Ready, 若最后一个清单已完成则转为 Completed.
缓存持久化, 只有用户显式删除才执行 End. 服务关闭仅停止任务并回收资源,
未完成缓存和已发布前缀均保留, 重启后恢复并等待显式构建或续接.
无效实例保留原文件并明确报错, 不自动删除. 临时工作文件清理和损坏尾部修复不属于整实例删除.

#

> 2026-11-02T14:18:37+09:00
