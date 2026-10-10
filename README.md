# Anime Server

用于 Linux 宿主机的本地视频 Web 服务. Rust/Axum 提供业务接口和 FFmpeg 播放会话,
SQLite 保存文件信息和进度, Vite/TypeScript 提供自定义播放器. 使用单个 Docker 容器部署.

## 播放方式

- 所有源视频统一转为 H.264 High, 8-bit `yuv420p`, 最高 1080p/60fps.
- 选中的音轨统一转为 AAC-LC, 双声道, 48 kHz. 无音轨时输出纯视频.
- 统一通过 fMP4 HLS 传输. Safari 使用原生 HLS, 其他支持 MSE 的浏览器使用 HLS.js.
- 只支持视频容器内的字幕和字体附件. 选中的 ASS/SSA, SRT 等文本字幕和 PGS 等位图字幕由服务器烧录.
- 切换音轨, 字幕或关闭字幕时切换缓存变体, 保留原视频时间位置及暂停/播放状态.
- 浏览器解析或解码失败时直接显示错误并释放会话, 不自动请求其他格式. Retry 只重复同一种输出.

播放器区域始终保留, 桌面布局右侧显示文件列表. 支持进度拖动, 音量, 全屏, 手动扫描,
主题配置和 SQLite 进度恢复. 独立进度条显示当前播放配置的缓存生成百分比和已生成时长,
生成期间每秒刷新, 与浏览器下载缓冲分开显示.
DandanPlay 识别目前仍是 mock, 弹幕字段和配置仅作预留.

目标是现代桌面浏览器和 iOS Safari, 不承诺所有旧设备均可播放. 本项目不做直放,
Remux, 自适应码率或客户端字幕渲染.

## 运行模型

Rust 播放配置包含文件前 16 MiB MD5, 视频轨, 音轨, 字幕轨和固定编码配置版本.
对配置的规范化 JSON 计算 MD5, 得到 `variant-key`, 对应一份持久 HLS 缓存.
客户端默认播放只提交文件 ID. 服务端读取数据库进度并选择默认音轨和容器内字幕,
返回实际轨道选择, 目标位置和不透明会话 ID, 不向客户端暴露缓存键.
当前有客户端使用的配置进入按键去重的活动集合, 共享一份 HLS 和一个生成任务.

空缓存从时间 0 顺序生成约 4 秒的分片. 只有目标位置严格小于已验证范围终点时才能播放,
相等时仍需等待. 拖动进度条只更新会话目标, 不创建跳转位置的独立任务.
最后一个客户端停止, 换流, 关闭页面或过期后, 编码器最多再提交一个完整分片便停止;
等待超过 30 秒则强制回收, 排队和字幕准备则立即取消. 部分缓存保留在磁盘上,
再次点播从已验证尾部续接, 不重编码已有视频分片. 完整缓存直接复用.

默认最多 2 个媒体进程, 其中为分片解码校验预留一个名额, 因此最多一个编码器.
基础编码线程数为 2, 目标未覆盖时启动任务可申请最多 8 个线程,
所有编码器共享默认 16 线程预算. 运行中不调整线程数或重启任务.
启动恢复修复完整和部分缓存, 但不会自动开始编码.

`playback.cache_max_bytes` 默认 `10737418240` 字节, 即 10 GiB.
LRU 按最后使用时间淘汰活动集合外的完整或部分缓存, 不删除活动目录或尚在打开的媒体文件.
停止清理中的目录仍受保护. 受保护条目允许暂时超限, 容量配置不是文件系统硬配额.

媒体探测结果只保存在内存中, 重启后重新探测. 播放进度保存在 SQLite 中, 不受会话清理影响.

## 项目结构

```text
anime-server/
|-- backend/
|   |-- src/
|   |   |-- main.rs
|   |   |-- api.rs           # 业务路由及 api/ 子模块
|   |   |-- db.rs            # db/ 子模块
|   |   |-- tasks.rs         # tasks/ 子模块
|   |   |-- video.rs         # video/ 子模块
|   |   |-- api/
|   |   |-- db/
|   |   |-- tasks/
|   |   `-- video/           # 探测, 烧录, 共享缓存及会话租约
|   |-- sql/init.sql
|   `-- tests/
|-- frontend/
|   |-- src/                 # API, 播放控制, HLS 适配及 UI
|   |-- index.html
|   `-- style.css
|-- documents/
|-- plan/
|-- examples/
|-- docker-compose.yml
`-- Dockerfile
```

Rust 目录模块使用同名 `.rs` 文件声明, 不使用 `mod.rs`.

## 挂载与端口

| 宿主机目录 | 容器目录   | 用途                             |
| ---------- | ---------- | -------------------------------- |
| 配置目录   | `/config/` | 只读, 保存 `config.json`         |
| 视频目录   | `/media/`  | 只读, 保存视频                   |
| 数据目录   | `/data/`   | 读写, 保存数据库和 API 数据      |
| 缓存目录   | `/cache/`  | 读写, 保存播放缓存和渲染中间文件 |

```text
/data/
|-- main.db
|-- ddp-ani/
`-- ddp-danmaku/

/cache/
|-- playback/
|   `-- <variant-key>/
|       |-- complete
|       |-- index.m3u8
|       |-- init.mp4
|       `-- segment_*.m4s
|-- subtitles/
`-- fonts/
```

`ddp-ani/` 和 `ddp-danmaku/` 是预留的 API 缓存目录.
`complete` 是成功生成并验证后的空标记文件, 没有额外 JSON 状态文件.
临时字幕和任务独立字体目录在生成结束后立即清理, 不提供浏览器下载接口.

容器固定监听 `8080`, 宿主机端口由根目录 `.env` 控制.

## 快速启动

```bash
cp examples/.env.example .env
```

编辑 `.env`, 使用宿主机绝对路径:

```dotenv
PORT=56080
MEDIA_DIR=/srv/media
CONFIG_DIR=/srv/anime-server/config
DATA_DIR=/srv/anime-server/data
CACHE_DIR=/srv/anime-server/cache
```

准备目录和配置, 然后启动:

```bash
mkdir -p /srv/anime-server/config /srv/anime-server/data /srv/anime-server/cache
cp examples/config.example.json /srv/anime-server/config/config.json
docker compose up -d --build
docker compose ps
```

打开 `http://localhost:56080`.

```bash
docker compose logs -f anime-server
docker compose down
```

删除容器不会删除宿主机数据库, 视频或完整和部分播放缓存.
Compose 为正常关闭预留 40 秒, 覆盖 30 秒分片停止期限和清理时间.
缓存目录使用独占锁, 不能同时供多个服务实例使用.
当前服务没有用户鉴权, 不要直接暴露到公网.

## 文档

| 文档                                       | 内容                                             |
| ------------------------------------------ | ------------------------------------------------ |
| [development.md](documents/development.md) | 开发, 调试, 编译, 部署, 验证和故障排查 |
| [api.md](documents/api.md)                 | 全部 HTTP API, 请求响应示例和错误码 |
| [database.md](documents/database.md)       | 数据库结构, 文件标识和进度持久化                 |
| [playback-cache.md](plan/playback-cache.md) | 活动缓存, 顺序续接和线程分配约定                 |
