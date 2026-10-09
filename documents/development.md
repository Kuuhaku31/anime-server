# 开发, 调试, 编译与部署

本文档说明 Anime Server
的开发环境准备, 本地检查, 调试方法, 编译流程, Docker 部署, 数据维护和常见故障处理.

## 技术组成

| 部分     | 技术                                |
| -------- | ----------------------------------- |
| 后端     | Rust, Axum, Tokio, SQLx             |
| 前端     | TypeScript, HTML, CSS               |
| 数据库   | SQLite                              |
| 容器     | Docker, Docker Compose              |
| 构建镜像 | Node.js 22, Rust 1, Debian Bookworm |

后端 crate 位于 `backend/`, 前端位于 `frontend/`, 数据库初始化 SQL 位于 `backend/sql/`.

## 环境要求

推荐安装以下工具:

- Docker.
- Docker Compose v2.
- Rust stable toolchain.
- Node.js 22 或兼容版本.
- npm.
- curl.

检查工具:

```bash
docker version
docker compose version
rustc --version
cargo --version
node --version
npm --version
curl --version
```

## 首次配置

### 创建 `.env`

从示例文件创建项目根目录的 `.env`:

```bash
cp examples/.env.example .env
```

编辑 `.env`:

```dotenv
PORT=56080
MEDIA_DIR=/absolute/path/to/videos
CONFIG_DIR=/absolute/path/to/config
DATA_DIR=/absolute/path/to/data
```

所有目录使用宿主机绝对路径.
使用 `sudo docker compose` 时不要使用 `~`, 因为 `~` 可能被解析为 `/root`.

变量说明:

| 变量         | 说明                              |
| ------------ | --------------------------------- |
| `PORT`       | 宿主机访问端口, 映射到容器 `8080` |
| `MEDIA_DIR`  | 视频目录, 只读挂载到 `/media`     |
| `CONFIG_DIR` | 配置目录, 只读挂载到 `/config`    |
| `DATA_DIR`   | 数据目录, 读写挂载到 `/data`      |

### 创建应用配置

```bash
cp examples/config.example.json /absolute/path/to/config/config.json
```

## 4. 推荐开发流程

当前程序使用固定容器路径 `/config`, `/media`, `/data` 和 `/app/web`.
因此完整功能开发推荐直接使用 Docker Compose.

常用命令:

```bash
# 启动开发容器
docker compose up -d --build
# 查看状态
docker compose ps
# 查看日志
docker compose logs -f anime-server
# 修改源码后重新构建
docker compose up -d --build
# 只重新构建镜像
docker compose build
# 停止服务
docker compose down
```

容器使用宿主机的配置, 视频和数据目录.
删除容器不会删除这些目录中的文件.

## 后端开发

### 代码检查

在项目根目录运行:

```bash
cargo check --locked
```

`--locked` 要求使用 `Cargo.lock` 中记录的依赖版本.
如果 `Cargo.toml` 和 `Cargo.lock` 不一致, 命令会失败.

### 编译

```bash
cargo build --locked -p anime-server
```

输出文件:

```text
target/debug/anime-server
```

### Release 编译

```bash
cargo build --release --locked -p anime-server
```

输出文件:

```text
target/release/anime-server
```

### 本地运行限制

直接执行以下命令:

```bash
cargo run --locked -p anime-server
```

程序会访问:

```text
/config/config.json
/media/
/data/main.db
/app/web/
```

配置文件路径可以覆盖:

```bash
ANIME_SERVER_CONFIG=/absolute/path/to/config.json cargo run --locked -p anime-server
```

数据库, 视频和前端静态文件路径当前不能通过环境变量覆盖.
如果不准备这些系统路径, 应使用 Docker Compose 调试完整服务.

## Docker 编译

使用 Compose:

```bash
docker compose build
```

直接构建:

```bash
docker build -t anime-server:dev .
```

Dockerfile 使用多阶段构建:

1. Node.js 阶段编译 TypeScript.
2. Rust 阶段使用 `cargo build --release --locked`.
3. Debian 运行阶段只复制后端二进制和前端静态文件.

检查镜像:

```bash
docker image inspect anime-server:dev
```
