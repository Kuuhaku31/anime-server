# Anime Server

用于 Linux 宿主机的本地视频 Web 服务. Rust/Axum 提供业务接口和 FFmpeg 播放会话,
SQLite 保存文件信息和进度, Vite/TypeScript 提供自定义播放器.
使用单个 Docker 容器部署.

## 项目结构

```text
anime-server/
|-- backend/
|   |-- src/
|   |   |-- main.rs
|   |   |-- api.rs           # 业务路由及 api/ 子模块
|   |   |-- db.rs            # db/ 子模块
|   |   |-- scanner.rs       # 媒体目录扫描与索引同步
|   |   |-- sessions.rs      # 播放会话与共享缓存租约
|   |   |-- cache.rs         # 元数据和活动播放缓存管理入口
|   |   |-- process.rs       # ProcessManager 及 process/ 子模块
|   |   |-- api/
|   |   |-- db/
|   |   |-- cache/
|   |   |   |-- metadata.rs  # 媒体探测, 成功结果缓存及过期策略
|   |   |   `-- disk.rs      # 磁盘占用统计及 LRU 访问时间持久化
|   |   |-- process/
|   |   |   |-- ffmpeg.rs    # FFmpeg 命令, HLS 读写, 恢复及解码校验
|   |   |   |-- assets.rs    # 临时字幕与字体资源
|   |   |   |-- fragment.rs  # fMP4 分片校验与时间轴归一化
|   |   |   `-- models.rs    # EncodeSpec 等媒体执行参数
|   |   `-- tests/           # 全部单元测试及测试夹具, 结构对应业务模块
|   |-- sql/init.sql
|   `-- tests/
`-- frontend/
    |-- src/                 # API, 播放控制, HLS 适配及 UI
    |-- index.html
    `-- style.css
```

## 挂载与端口

| 宿主机目录 | 容器目录   | 用途                       |
| ---------- | ---------- | -------------------------- |
| 配置目录   | `/config/` | 只读, 保存 `config.json`   |
| 视频目录   | `/media/`  | 只读, 保存视频             |
| 数据目录   | `/data/`   | 保存数据库 `main.db`       |
| 缓存目录   | `/cache/`  | 保存播放缓存和渲染中间文件 |

```
/cache/
`-- <variant-key>/
    |-- subtitles/
    |-- fonts/
    |-- complete
    |-- index.m3u8
    |-- init.mp4
    `-- segment_*.m4s
```

`ddp-ani/` 和 `ddp-danmaku/` 是预留的 API 缓存目录.
`complete` 是成功生成并验证后的空标记文件.
临时字幕和任务独立字体目录在生成结束后立即清理.

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

## 文档

| 文档                                         | 内容                                   |
| -------------------------------------------- | -------------------------------------- |
| [development.md](documents/development.md)   | 开发, 调试, 编译, 部署, 验证和故障排查 |
| [architecture.md](documents/architecture.md) | 六个核心后端模块的职责与调用关系       |
| [api.md](documents/api.md)                   | 全部 HTTP API, 请求响应示例和错误码    |
| [database.md](documents/database.md)         | 数据库结构, 文件标识和进度持久化       |
| [playback-cache.md](plan/playback-cache.md)  | 活动缓存, 顺序续接和线程分配约定       |
