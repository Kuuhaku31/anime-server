# 开发, 部署与验证

## 环境与约定

后端使用 Rust, Tokio, Axum 和 SQLx, 前端使用 TypeScript, Vite 和 HLS.js.
完整播放需要 FFmpeg, ffprobe, libass, libx264, AAC, HDR 转换滤镜和 Noto CJK 字体.
Dockerfile 会检查和安装运行依赖, 启动缺少必要能力时明确失败, 不降级输出.

根目录 Cargo.toml 定义 workspace, backend/Cargo.toml 定义二进制 crate.
源码职责与数据流见 [architecture.md](architecture.md).
不运行格式化工具, 不在未经确认时升级项目依赖.
构建使用 Cargo.lock 和 frontend/package-lock.json 锁定版本.

源文件以空行, 相对路径注释和功能注释开头.
新增类型, 字段和函数使用简短中文说明, 注释不用全角标点.
公开接口在前, 私有辅助逻辑在后, 可见性限制为必要的 pub(crate) 或 pub(super).

## 四目录部署

从 `examples/.env.example` 创建根目录 `.env`, 使用宿主机绝对路径:

```dotenv
PORT=56080
MEDIA_DIR=/srv/media
CONFIG_DIR=/srv/anime-server/config
DATA_DIR=/srv/anime-server/data
CACHE_DIR=/srv/anime-server/cache
```

配置只读挂载为 `/config`, 媒体只读挂载为 `/media`,
数据读写挂载为 `/data`, 播放缓存通过 CACHE_DIR 独立读写挂载为 `/cache`.
端口映射为 `${PORT}:8080`, PORT 必须在 `1..65535` 内.
CACHE_DIR 必填且使用宿主机绝对路径, 后端固定使用容器内的 `/cache`.

```bash
mkdir -p /srv/anime-server/config /srv/anime-server/data /srv/anime-server/cache
cp examples/config.example.json /srv/anime-server/config/config.json
docker compose config --quiet
docker compose up -d --build
docker compose ps
curl --fail http://localhost:56080/health
```

不要让多个进程使用相同 DATA_DIR 或 CACHE_DIR. 当前没有缓存目录锁, 隔离由部署保证.
如果用户没有 Docker socket 权限, 使用宿主机已授权的 Docker 访问方式,
不要为方便调试修改 socket 为全员可写.

本次架构不兼容旧配置, 数据库和 API.
先停止并备份旧实例, 再用示例配置和空数据库重新扫描,
不要直接让测试实例操作生产数据.
已知哈希命名但缺少有效 instance.json 的旧缓存会在启动时清理.

## 运行配置

`/config/config.json` 为扁平对象:

```json
{
  "default_dark_mode": true,
  "scan_interval_seconds": 300,
  "max_processes": 2,
  "ffmpeg_threads": 2,
  "max_total_ffmpeg_threads": 16,
  "startup_timeout_seconds": 90
}
```

| 字段                     | 约束和含义                                    |
| ------------------------ | --------------------------------------------- |
| default_dark_mode        | 前端默认主题, 默认 true                       |
| scan_interval_seconds    | 正整数, 后台扫描间隔秒数, 默认 300            |
| max_processes            | 2..16, 媒体探测, 编码和校验共用名额           |
| ffmpeg_threads           | 1..32, 单个编码任务使用的固定线程数           |
| max_total_ffmpeg_threads | ffmpeg_threads..256, 所有编码任务共享线程预算 |
| startup_timeout_seconds  | 10..600, 字幕准备及首个可发布结果的超时       |

缺省字段使用默认值, 未知字段报错.
旧 web/playback 嵌套对象, cache_max_bytes, idle_timeout_seconds 和 max_variant_threads 已删除.
配置变更需要重启服务.

编码任务名额为 max_processes - 1, 至少留一个位置供分片校验.
默认两个进程名额允许一个编码器与一个校验进程.
预算不足时排队, 不根据用户等待位置动态提高线程数.
线程预算指编码线程, 不是解码器, 滤镜或系统全部线程的硬上限.

## 构建

```bash
CARGO_TARGET_DIR=/tmp/target cargo check --locked --manifest-path backend/Cargo.toml
CARGO_TARGET_DIR=/tmp/target cargo build --locked --manifest-path backend/Cargo.toml
CARGO_TARGET_DIR=/tmp/target cargo test --locked --manifest-path backend/Cargo.toml
npm test --prefix frontend
npm run build --prefix frontend
```

只有缺少前端依赖时才使用 `npm ci --prefix frontend` 按锁文件安装.
不运行依赖升级命令. Node.js 需兼容 Vite 8 与测试脚本的 TypeScript strip-types.
Docker 构建使用 Node 22 和 Rust 镜像.

前端 build 先执行 TypeScript 检查, 再构建 frontend/dist 并复制 HLS.js 许可证.
若 node_modules/.vite-temp 或 dist 曾被 root 创建, 普通用户可能不能写入.
可先使用独立输出目录定位问题, 不必重装或升级依赖:

```bash
cd frontend
./node_modules/.bin/tsc --noEmit
./node_modules/.bin/vite build --configLoader runner --outDir /tmp/anime-server-web-check
```

不使用 `--emptyOutDir` 指向共享或未知目录.
这只是构建验证替代路径, 部署仍通过 Dockerfile 生成 `/app/web`.

## 开发服务

```bash
docker compose up -d
npm run dev --prefix frontend
```

Vite 默认地址为 `http://localhost:5173`, `/api`, `/playback`, `/health`
代理到 `http://127.0.0.1:56080`. 改了宿主机端口时同步修改 frontend/vite.config.ts.

直接运行 Rust 仍使用固定 `/media`, `/data`, `/cache`, `/app/web` 和端口 8080,
只允许通过 ANIME_SERVER_CONFIG 覆盖配置文件地址.
没有准备这些目录时优先在容器中联调, 不修改共享宿主机目录以迁就测试.

## 测试组织

所有 Rust 测试集中在 backend/src/tests, 与业务模块路径对应.
二进制 crate 通过被测模块的 cfg(test) 子模块访问私有实现,
业务文件只保留测试模块声明. 例如 cache/handle.rs 使用:

```rust
// 仅在测试构建中加载集中保存的句柄回归测试.
#[cfg(test)]
#[path = "../tests/cache/handle.rs"]
mod tests;
```

tests/fixtures/runtime.rs 提供隔离临时目录和合成媒体.
单元测试直接构造独立 Manager, 不注册或重置 OnceLock 全局实例.
涉及 FFmpeg 的缓存测试使用短小合成媒体, 不读取用户的视频目录.
测试通过 cfg(test) 编译, 不进入生产二进制.

覆盖内容包括稳定身份, 缺省/null 选轨, 数据库多副本与路径替换,
扫描边界, 状态矩阵, 并发打开, 排队取消, Failed 重试,
完成缓存重启恢复, 损坏尾部修复和不覆盖前缀的真实续接.
前端测试覆盖路径契约, 暂停状态换轨, 小数 seek, 半开区间等待,
停止和解码失败释放, 本地目标不发送到服务端, 完成后无需心跳.

## Docker 验证

验证必须使用全新数据目录, 缓存目录和独立容器, 不复用实际部署的数据库或缓存.
以下示例中的目录需要提前准备合成媒体和示例配置:

```bash
docker build -t anime-server:verify .
docker run -d --name anime-server-verify \
  -p 127.0.0.1:56081:8080 \
  -v /tmp/anime-server-verify/config:/config:ro \
  -v /tmp/anime-server-verify/media:/media:ro \
  -v /tmp/anime-server-verify/data:/data \
  -v /tmp/anime-server-verify/cache:/cache \
  anime-server:verify
curl --fail http://127.0.0.1:56081/health
curl --fail -X POST http://127.0.0.1:56081/api/media/scan
curl --fail http://127.0.0.1:56081/api/media
```

创建播放只传 path 和可选流索引, 再查询返回的 session_id:

```bash
curl --fail http://127.0.0.1:56081/api/playback \
  -H 'Content-Type: application/json' \
  -d '{"path":"/media/sample.mkv","subtitle_stream_index":null}'
curl --fail http://127.0.0.1:56081/api/playback/SESSION_ID
```

验证缓存进入 completed, 清单含 ENDLIST, 每个分片可以配合 init 真实解码.
同内容不同路径应共享一份 instance.json 和输出, 但返回不同 session_id.
删除其中一个 session 后另一个仍可读取资源, 缓存继续生成.

保存配置后重启测试容器, 确认数据库配置和 completed 缓存保留,
旧 session 消失且新会话复用缓存.
在测试实例上验证失败重试和部分尾部恢复, 不破坏生产缓存.
浏览器验证播放时间前进, 视频/音频/字幕切换, 暂停意图和目标范围等待.
Safari 原生 HLS 的最终兼容性仍需真实设备验证.

```bash
docker logs anime-server-verify
docker stop --time 40 anime-server-verify
docker rm anime-server-verify
```

## 播放与资源生命周期

所有输入统一转为 H.264 High, 8-bit yuv420p, 最高 1920x1080,
帧率为源声明帧率与 60 的较小值, HDR 转 BT.709 SDR.
可选音轨为双声道 48 kHz AAC-LC.
文本字幕使用 libass 烧录, 位图字幕使用 overlay, 不加载外挂字幕.

首次从零生成, 续接只编码未缓存视频尾部.
音频和位图字幕需要读取历史以恢复状态, 但不重新编码已有视频.
视频时基为 90000, 音频时基为 48000, 禁用 B 帧,
分片只有通过时间轴和真实解码校验后才进入正式清单.

每个 `.work-<random_id>/` 包含本次生成的临时 HLS, subtitle.ass 和 fonts/.
成功, 失败或取消后清理该目录, 异常退出的遗留目录在恢复时清理.
进程 stderr 只保留末尾诊断, 不无限累积转码日志.

缓存与会话生命周期解耦. session 释放不会停止编码,
没有自动过期, LRU 或缓存容量设置.
Ready 和 Completed 等待复用, 仅显式删除, 无效实例清理或关闭流程执行 End.
服务关闭先停止 HTTP, 再停止后台任务和缓存写入, 最后关闭数据库.

## 故障排查

| 现象                | 处理                                                 |
| ------------------- | ---------------------------------------------------- |
| 旧数据库启动失败    | 停止旧实例并备份, 换全新数据库重新扫描               |
| 旧配置字段报错      | 使用新的扁平示例配置, 删除已取消字段                 |
| 列表探测错误        | 查看服务日志中的具体路径, 修复或移除不可读媒体后重扫 |
| 等待目标较慢        | 新缓存从零连续生成, 可拖回已生成范围                 |
| failed 且目标未覆盖 | 查看 FFmpeg 诊断, 修复媒体或环境后手动 Retry         |
| 字幕没有字形        | 检查内嵌字体和镜像的 Noto CJK 回退字体               |
| 会话不存在          | 服务重启或已释放, 手动重新创建播放                   |
| 缓存目录无法初始化  | 检查写权限, 未知条目和是否有其他实例共享目录         |
| 磁盘空间不足        | 没有自动淘汰, 停服后仅清理明确需要移除的缓存实例     |
| 构建目录权限错误    | 使用独立输出目录或按宿主机策略修复指定目录所有权     |
