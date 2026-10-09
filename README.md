# Anime Server

Anime Server 是一个用于扫描, 管理和播放本地视频文件的 Web 服务.
后端使用 Rust 和 Axum, 数据库使用 SQLite, 前端使用 TypeScript,
整体以单个 Docker 容器运行.

## 功能

- 自动扫描视频目录并计算文件前 16 MiB 的 MD5.
- 在浏览器中列出和播放本地视频.
- 支持 HTTP Range 请求, 可拖动视频播放进度.
- 保存和恢复每个视频的播放进度.
- 支持手动触发视频扫描.
- 支持通过配置文件设置默认主题.
- 保留 DandanPlay 识别和弹幕功能的配置接口.

## 项目结构

```text
anime-server/
├── backend/                  # Rust 后端
│   └── src/
│       ├── api/              # HTTP API
│       ├── db/               # SQLite 数据模型和存储
│       ├── tasks/            # 后台扫描任务
│       └── video/            # 视频扫描和文件指纹
├── frontend/                 # TypeScript 前端
│   ├── src/
│   ├── index.html
│   └── style.css
├── documents/                # 项目文档
├── backend/sql/              # SQLite schema
├── examples/                 # 配置示例
├── docker-compose.yml
└── Dockerfile
```

## 容器目录

容器只使用 3 个宿主机挂载目录:

| 宿主机目录 | 容器目录   | 用途                        |
| ---------- | ---------- | --------------------------- |
| 配置目录   | `/config/` | 只读, 保存配置文件          |
| 视频目录   | `/media/`  | 只读, 保存视频文件          |
| 数据目录   | `/data/`   | 读写, 保存数据库和 API 缓存 |

数据目录结构:

```text
/data/
├── main.db
├── ddp-ani/
└── ddp-danmaku/
```

容器固定监听 `8080` 端口. 宿主机端口通过 Compose 配置.

## 快速启动

1. 创建本地环境变量文件:

```bash
cp examples/.env.example .env
```

2. 编辑 `.env`, 所有目录建议使用绝对路径:

```dotenv
PORT=YOUR_PORT
MEDIA_DIR=YOUR_MEDIA_DIR
CONFIG_DIR=YOUR_CONFIG_DIR
DATA_DIR=YOUR_DATA_DIR
```

3. 创建宿主机目录并复制应用配置:

```bash
mkdir -p YOUR_CONFIG_DIR
mkdir -p YOUR_DATA_DIR
cp examples/config.example.json YOUR_CONFIG_DIR/config.json
```

4. 构建并启动:

```bash
docker compose up -d --build
```

5. 打开 `http://localhost:YOUR_PORT`.

查看状态和日志:

```bash
docker compose ps
docker compose logs -f
```

停止服务:

```bash
docker compose down
```

## 本地开发

完整开发, 调试, 编译和部署流程见 [documents/development.md](documents/development.md).

后端:

```bash
cargo check
cargo run -p anime-server
```

后端本地运行时使用固定路径 `/config`, `/media` 和 `/data`.

前端:

```bash
npm install --prefix frontend
npm run build --prefix frontend
```

前端构建结果输出到 `frontend/dist/`.

## HTTP 接口

| 方法   | 路径                                 | 说明                   |
| ------ | ------------------------------------ | ---------------------- |
| `GET`  | `/health`                            | 健康检查               |
| `GET`  | `/api/config`                        | 获取 Web 配置          |
| `GET`  | `/api/files`                         | 获取视频列表           |
| `GET`  | `/api/files/{pre16mib_md5}`          | 获取视频详情和播放地址 |
| `POST` | `/api/files/{pre16mib_md5}/identify` | 执行当前 mock 识别     |
| `GET`  | `/api/files/{pre16mib_md5}/progress` | 获取播放进度           |
| `PUT`  | `/api/files/{pre16mib_md5}/progress` | 保存播放进度           |
| `POST` | `/api/scan`                          | 手动扫描视频目录       |
| `GET`  | `/media/*`                           | 读取视频文件           |

## 数据持久化

镜像本身不保存运行数据.
配置, 视频, SQLite 数据库和 API 缓存均位于宿主机挂载目录.

文件信息和文件地址分开保存.
即使某个文件地址暂时不可用, 已保存的识别信息和播放进度仍可保留.

# 文档

| 文档                                       | 内容                                                  |
| ------------------------------------------ | ----------------------------------------------------- |
| [development.md](documents/development.md) | 开发环境, 调试, 编译, Docker 部署, 数据维护和故障排查 |
| [database.md](documents/database.md)       | SQLite 数据库结构                                     |
