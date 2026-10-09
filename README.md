# Anime Server

用于 Linux 宿主机的本地视频 Web 服务. Rust/Axum 提供业务接口和 FFmpeg 播放会话,
SQLite 保存文件信息和进度, Vite/TypeScript 提供自定义播放器. 使用单个 Docker 容器部署.

## 播放方式

- 所有源视频统一转为 H.264 High, 8-bit `yuv420p`, 最高 1080p/60fps.
- 选中的音轨统一转为 AAC-LC, 双声道, 48 kHz. 无音轨时输出纯视频.
- 统一通过 fMP4 HLS 传输. Safari 使用原生 HLS, 其他支持 MSE 的浏览器使用 HLS.js.
- 选中的 ASS/SSA, SRT 等文本字幕和 PGS 等位图字幕统一由服务器烧录, 支持内嵌字体和外挂文本字幕.
- 切换音轨, 切换字幕或关闭字幕时重新建流, 保留原视频时间位置及暂停/播放状态.
- 浏览器解析或解码失败时直接显示错误并释放会话, 不自动请求其他格式. Retry 只重复同一种输出.

播放器区域始终保留, 桌面布局右侧显示文件列表. 支持进度拖动, 音量, 全屏, 手动扫描,
主题配置和 SQLite 进度恢复. DandanPlay 识别目前仍是 mock, 弹幕字段和配置仅作预留.

目标是现代桌面浏览器和 iOS Safari, 不承诺所有旧设备均可播放. 本项目不做直放,
Remux, 自适应码率或客户端字幕渲染.

## 运行模型

每个播放会话独占一个 FFmpeg 转码进程和临时目录. 不去重任务, 不复用已经生成的流.
播放结束, 停止, 换流或关闭页面时释放会话; 无客户端保活时超时清理.
服务启动时清理上次遗留的会话目录.

默认最多使用 2 个媒体处理名额, 每个转码进程使用 2 个编码线程.
首次播放和每次换轨都需要等待首段生成. 始终重编码会消耗 CPU,
长视频在活跃会话期间也可能占用较多临时磁盘空间.

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
|   |   `-- video/           # 探测, 烧录, 编码及临时会话
|   |-- sql/init.sql
|   `-- tests/
|-- frontend/
|   |-- src/                 # API, 播放控制, HLS 适配及 UI
|   |-- index.html
|   `-- style.css
|-- documents/
|-- examples/
|-- docker-compose.yml
`-- Dockerfile
```

Rust 目录模块使用同名 `.rs` 文件声明, 不使用 `mod.rs`.

## 挂载与端口

| 宿主机目录 | 容器目录 | 用途 |
| --- | --- | --- |
| 配置目录 | `/config/` | 只读, 保存 `config.json` |
| 视频目录 | `/media/` | 只读, 保存视频和外挂字幕 |
| 数据目录 | `/data/` | 读写, 保存数据库及临时播放数据 |

```text
/data/
|-- main.db
|-- ddp-ani/
|-- ddp-danmaku/
`-- playback-sessions/
    `-- <session-id>/
        |-- index.m3u8
        |-- init.mp4
        |-- segment_*.m4s
        |-- subtitle.ass     # 仅文本字幕会话
        `-- fonts/           # 仅服务端烧录使用
```

`ddp-ani/` 和 `ddp-danmaku/` 是预留的 API 缓存目录.
`playback-sessions/` 下的内容不是持久缓存, 会话释放后删除.
字幕和字体不提供浏览器下载接口.

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
```

准备目录和配置, 然后启动:

```bash
mkdir -p /srv/anime-server/config /srv/anime-server/data
cp examples/config.example.json /srv/anime-server/config/config.json
docker compose up -d --build
docker compose ps
```

打开 `http://localhost:56080`.

```bash
docker compose logs -f anime-server
docker compose down
```

删除容器不会删除宿主机数据库或视频. 当前服务没有用户鉴权, 不要直接暴露到公网.

## 文档

| 文档 | 内容 |
| --- | --- |
| [development.md](documents/development.md) | 开发, 调试, 编译, 部署, 播放接口, 验证和故障排查 |
| [database.md](documents/database.md) | 数据库结构, 文件标识和进度持久化 |
