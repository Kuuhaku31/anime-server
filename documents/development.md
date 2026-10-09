# 开发, 调试, 编译与部署

## 技术与目录

| 部分     | 技术                                   |
| -------- | -------------------------------------- |
| 后端     | Rust, Axum, Tokio, SQLx                |
| 前端     | TypeScript, Vite, 原生 video, HLS.js   |
| 媒体处理 | FFmpeg, ffprobe, libass, Noto CJK 字体 |
| 数据库   | SQLite                                 |
| 部署     | 单个 Docker 容器, Docker Compose       |

根目录 `Cargo.toml` 定义 workspace, `backend/Cargo.toml` 定义后端 crate.
后端入口是 `backend/src/main.rs`. `api.rs`, `db.rs`, `tasks.rs`, `video.rs`
声明各自同名目录下的子模块, 不使用 `mod.rs`.
数据库初始化文件是 `backend/sql/init.sql`.

前端入口是 `frontend/src/main.ts`, 构建结果位于 `frontend/dist/`.
后端从容器内 `/app/web` 提供静态文件, 前后端使用同源 HTTP 接口通信.

## 环境准备

完整运行推荐使用 Docker Compose. 本地编译还需要 Rust stable, Node.js 22.12 或更新的兼容版本,
npm. 生成测试媒体需要本机安装 FFmpeg 和 ffprobe.

```bash
docker version
docker compose version
rustc --version
cargo --version
node --version
npm --version
```

项目使用 `Cargo.lock` 和 `frontend/package-lock.json` 锁定依赖.
不要运行格式化工具或未经确认的依赖升级命令.

## 首次部署

```bash
cp examples/.env.example .env
```

编辑 `.env`:

```dotenv
PORT=56080
MEDIA_DIR=/srv/media
CONFIG_DIR=/srv/anime-server/config
DATA_DIR=/srv/anime-server/data
CACHE_DIR=/srv/anime-server/cache
```

| 变量         | 用途                                                    |
| ------------ | ------------------------------------------------------- |
| `PORT`       | 宿主机端口, 映射到容器 `8080`, 必须在 `1..65535` 范围内 |
| `MEDIA_DIR`  | 视频目录, 只读挂载到 `/media`                           |
| `CONFIG_DIR` | 配置目录, 只读挂载到 `/config`                          |
| `DATA_DIR`   | 数据目录, 读写挂载到 `/data`                            |
| `CACHE_DIR`  | 播放缓存目录, 读写挂载到 `/cache`, 必须指定             |

使用绝对路径, 避免 `~` 在不同用户或 sudo 环境中展开到不同目录.
先确保视频目录存在, 再准备配置, 数据和缓存目录:

```bash
mkdir -p /srv/anime-server/config /srv/anime-server/data /srv/anime-server/cache
cp examples/config.example.json /srv/anime-server/config/config.json
docker compose config
docker compose up -d --build
docker compose ps
curl --fail http://localhost:56080/health
```

容器启动时检查 FFmpeg 编码器, 字幕滤镜, ffprobe, 字体和缓存目录写权限.
`/health` 只返回服务存活状态, 不执行媒体探测或磁盘容量统计.

修改端口或挂载后需要重新创建容器:

```bash
docker compose up -d --force-recreate
docker compose port anime-server 8080
```

不要同时让多个服务实例使用同一个 `DATA_DIR` 或 `CACHE_DIR`.
缓存根目录使用独占锁, 第二个实例会明确报错. 服务启动时保留完整缓存,
删除未完成或无效的变体及遗留的字幕和字体中间文件.

## 应用配置

`/config/config.json` 示例:

```json
{
  "web": {
    "default_dark_mode": true
  },
  "scan_interval_seconds": 300,
  "playback": {
    "cache_max_bytes": 10737418240,
    "max_processes": 2,
    "ffmpeg_threads": 2,
    "idle_timeout_seconds": 60,
    "startup_timeout_seconds": 90
  },
  "dandanplay": {
    "enabled": false,
    "api_base": ""
  }
}
```

| 播放配置                  | 说明                                                                       |
| ------------------------- | -------------------------------------------------------------------------- |
| `cache_max_bytes`         | 缓存容量字节数, 正整数 `u64`, 默认 `10737418240`, 即 10 GiB                 |
| `max_processes`           | 媒体处理名额, 范围 `1..16`. 播放准备及编码占用一个名额, 外部探测请求也受限 |
| `ffmpeg_threads`          | 每个转码进程的编码线程数, 范围 `1..32`                                     |
| `idle_timeout_seconds`    | 无保活请求后的会话过期时间, 范围 `20..3600` 秒                             |
| `startup_timeout_seconds` | 字幕准备及首段生成各阶段的超时, 范围 `10..600` 秒                          |

缺少 `playback` 时使用上述默认值, 未知的播放配置字段会报错.
修改应用配置后重启容器. DandanPlay 当前未实现真实 API 调用.
Web 界面在缓存状态及生成范围连续 180 秒没有进展时显示错误.
生成正常推进时允许继续等待较远的目标位置, 不设置 180 秒总时限.
HLS 元数据加载最多等待 30 秒, 超时后显示错误并释放租约, 不取消后台生成.

## 开发与编译

后端:

```bash
cargo check --locked
cargo test --locked -p anime-server
cargo build --locked -p anime-server
cargo build --release --locked -p anime-server
```

产物分别是 `target/debug/anime-server` 和 `target/release/anime-server`.
`--locked` 禁止构建过程改写依赖版本.

前端首次安装和构建:

```bash
npm ci --prefix frontend
npm test --prefix frontend
npm run build --prefix frontend
```

前端热更新开发:

```bash
docker compose up -d
npm run dev --prefix frontend
```

打开 Vite 输出的地址, 默认是 `http://localhost:5173`.
`frontend/vite.config.ts` 将 `/api`, `/playback`, `/health` 代理到 `http://127.0.0.1:56080`.
如果修改了宿主机端口, 也需要调整此开发代理目标.

直接运行 Rust:

```bash
cargo run --locked -p anime-server
```

本地运行仍使用固定路径 `/media`, `/data`, `/cache`, `/app/web`, 并监听 `8080`.
只有配置文件路径可以通过 `ANIME_SERVER_CONFIG` 覆盖.
不准备这些路径时, 应在容器中联调.

## Docker 构建与调试

```bash
docker compose build
docker compose up -d
docker compose logs -f anime-server
docker compose exec anime-server ffmpeg -version
docker compose exec anime-server ffprobe -version
```

Dockerfile 分为 Node.js 前端构建, Rust 后端构建和 Debian 运行三个阶段.
运行镜像只包含后端程序, 前端静态文件, FFmpeg, 字体和必要运行依赖.
前端构建使用 `npm ci`, 后端使用 `cargo build --release --locked`.

源码修改后:

```bash
docker compose up -d --build
```

停止服务:

```bash
docker compose down
```

## 统一播放管线

所有输入都使用同一种输出, 包括本来可以在浏览器直接播放的 MP4:

| 项目         | 固定规则                                                 |
| ------------ | -------------------------------------------------------- |
| 视频         | libx264, High, 8-bit `yuv420p`, CRF 23, veryfast         |
| 分辨率与帧率 | 保持比例, 不放大小视频, 最高 1920x1080 和 60fps          |
| HDR          | 转为 BT.709 SDR                                          |
| 音频         | 选定一轨, AAC-LC, 双声道, 48 kHz, 192 kbps; 可选择无音轨 |
| 字幕         | 选定一轨服务端烧录, 或关闭字幕                           |
| 传输         | fMP4 HLS, 目标分段长度 4 秒, event playlist              |
| 浏览器       | Safari 原生 HLS, 其他支持 MSE 的浏览器使用 HLS.js        |

传输适配在加载前选定. Chromium 即使声明可能支持原生 HLS, 也优先使用 HLS.js,
避免将仍在生成的 fMP4 event playlist 交给不完整的原生实现.

只支持容器内字幕轨, 索引使用 ffprobe 的真实流索引. 不扫描或加载外挂字幕.
ASS/SSA 和其他文本字幕先提取到 `/cache/subtitles/<random-id>.ass`, 再由 libass 渲染.
内嵌字体提取到 `/cache/fonts/<generation-id>/`, 文件名独立随机生成;
缺少字体时使用运行镜像的回退字体. 位图字幕直接通过 overlay 烧录, 不提取 ASS.
所有生成任务从时间 0 开始, 保留字幕历史状态和源容器的时间偏移.
生成结束, 失败或服务关闭取消任务后, 立即删除中间字幕和字体.

切换音轨, 字幕或字幕关闭时:

1. 保存原始视频时间及暂停/播放状态.
2. 停止浏览器加载并释放旧会话.
3. 以选定轨道和原始时间创建新租约, 复用或开始生成对应缓存.
4. 等待缓存覆盖目标时间及少量播放余量, 然后附加新流.
5. 原先正在播放时继续播放, 原先暂停时保持暂停.

服务端不按请求位置执行 FFmpeg seek, 也不优先渲染跳转位置.
所有缓存的 `start_time` 固定为 0, 浏览器直接定位原视频时间,
实际画面仍受源视频帧间隔约束.
已生成范围内的 seek 可以直接使用当前流. 超出范围时保持同一租约,
等待顺序生成覆盖目标位置后重新加载 HLS. 等待时仍可拖动到另一个位置或停止播放.
浏览器阻止自动播放时会显示 `Press Play to start`, 需要用户点击播放.

浏览器解码错误, HLS 致命错误或分段解析错误会直接显示, 停止加载并释放会话.
不会请求另一种格式, 也不会自动重建播放会话.
用户点击 Retry 才重新请求同样的输出. 普通网络分片重试不改变格式.

## 播放 API

先读取轨道:

```bash
curl http://localhost:56080/api/files/FILE_ID/streams
```

使用返回的流索引:

```bash
curl -X POST http://localhost:56080/api/files/FILE_ID/playback-plan \
  -H 'Content-Type: application/json' \
  -d '{"video_stream_index":0,"audio_stream_index":1,"subtitle_stream_index":3,"position":125.5}'
```

响应:

```json
{
  "session_id": "SESSION_ID",
  "url": "/playback/SESSION_ID/index.m3u8",
  "mime_type": "application/vnd.apple.mpegurl",
  "start_time": 0.0,
  "duration": 1400.0,
  "state": "queued",
  "ready": false,
  "buffered_until": 0.0,
  "error": null
}
```

`audio_stream_index: null` 表示无音轨, `subtitle_stream_index: null` 表示关闭字幕.
缺省视频索引时选第一条视频轨. 时间必须为有限非负数且小于视频时长.
客户端不能指定缓存键, 路径或编码配置版本, 也不提交 `source_version`.
`position` 不参与缓存键计算, 只用于请求校验和前端恢复位置.

| 方法          | 路径                                | 说明                                   |
| ------------- | ----------------------------------- | -------------------------------------- |
| `GET`         | `/health`                           | 服务存活                               |
| `GET`         | `/api/config`                       | Web 配置                               |
| `GET`         | `/api/files`                        | 文件列表                               |
| `GET`         | `/api/files/{id}`                   | 文件详情及路径, 不返回原始媒体播放地址 |
| `POST`        | `/api/files/{id}/identify`          | mock 识别                              |
| `GET` / `PUT` | `/api/files/{id}/progress`          | 获取或保存原始视频进度                 |
| `POST`        | `/api/scan`                         | 扫描视频目录                           |
| `GET`         | `/api/files/{id}/streams`           | 进程内缓存的轨道信息                   |
| `POST`        | `/api/files/{id}/probe`             | 强制重新探测                           |
| `POST`        | `/api/files/{id}/playback-plan`     | 创建共享缓存的客户端租约               |
| `GET`         | `/api/playback/{session}/status`    | 获取状态, 同时保活                     |
| `POST`        | `/api/playback/{session}/heartbeat` | 显式保活                               |
| `DELETE`      | `/api/playback/{session}`           | 仅释放租约并触发容量清理, 可重复调用   |
| `GET`         | `/playback/{session}/{asset}`       | 只读取 manifest, init 和编号媒体分片   |

会话状态为 `queued`, `starting`, `ready`, `complete`, `failed` 或 `cancelled`.
只有 `ready: true` 才能开始读取 HLS. 前端准备阶段轮询状态,
播放阶段每 10 秒查询状态保活; 媒体请求也延长会话有效期.
`buffered_until` 是从时间 0 开始已经生成的范围终点, 不是浏览器本地缓冲长度.
`ready: true` 表示首段验证通过, 较后的目标位置仍可能需要等待.
会话释放或过期后, 完整缓存保留到 LRU 淘汰, 未完成的缓存继续生成.

非法请求返回 `400`, 无法探测的文件返回 `422`, 不存在或过期的会话返回 `404`.
异步字幕准备或编码失败由状态响应的 `error` 返回.

## 数据与资源管理

`/data/main.db` 只保存业务数据和进度. 媒体探测结果及探测错误只放在内存中,
按文件 ID 缓存, 显式重试或服务重启后可以重新探测.

Rust `PlaybackVariantConfig` 依次包含 `src_file_pre16mib_md5`, `video_stream`,
`audio_stream`, `subtitle_stream`, `encoder_profile`.
使用 `serde_json::to_vec` 按字段顺序生成紧凑 JSON, 保留空轨道的 `null`,
计算 MD5 后得到 32 位小写 `variant-key`. 固定编码配置为 `h264-high-yuv420p-aac-fmp4-v1`.
路径, 请求位置和文件修改时间不参与计算. 文件改名或移动可以复用缓存,
但仅修改前 16 MiB 之后的内容不会使缓存自动失效.
此时需停止服务后删除对应缓存, 或随编码规则调整更新服务端 `encoder_profile`.
该摘要只用于索引, 不提供鉴权.

```text
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

相同播放配置共享一份缓存及一个生成任务, 不同音轨或字幕配置可以同时存在.
每个客户端仍获得独立会话 ID 和媒体地址. 最多允许 32 个租约和 32 个排队或生成中的缓存任务.
Stop 只释放租约, 不终止 FFmpeg, 因而不会立即释放媒体处理名额.
服务关闭时取消并回收所有未完成的生成任务, 完整缓存保留.
强制终止留下的不完整输出及中间文件在下次启动时清理.

不保存 `metadata.json` 或单独的持久缓存索引.
FFmpeg 成功退出, 最终 playlist 和所有 MP4 分片结构校验通过,
且中间文件清理完成后, 原子创建空的 `complete` 标记.
重启时校验完成标记, playlist, 分片文件和首段编码, 只加载有效的完整缓存.
缓存命中不重新编码或提取字幕, 缺失或损坏的文件会明确报错.
启动清理仅针对服务生成的命名, 不删除未知名称的文件.
只保留 FFmpeg 最近 4000 字节 stderr 作为错误详情, 不写持久转码日志.

容量统计包含 HLS 和字幕, 字体中间文件的实际字节数.
最后使用时间记录在内存中, 并通过变体目录修改时间跨重启保留;
每 5 秒维护一次, 释放和生成完成时也执行维护, 避免每个分片请求都写盘.
超过 `cache_max_bytes` 时, 按 LRU 删除无租约的完整变体, 直到满足上限或没有可淘汰条目.
正在生成或使用中的变体不淘汰, 允许暂时超限. 超过上限的单个完整变体也会在无租约时被删除.
后台生成在无人观看时仍消耗 CPU 和磁盘, 必须为受保护的缓存预留额外空间.
不要在服务运行时手动删除缓存, 修改完成标记或接管中间文件.
备份数据库前建议停止容器, 并保留整个数据目录中的 SQLite 数据及可能存在的 WAL 文件.

## 验证

```bash
cargo test --locked -p anime-server
npm test --prefix frontend
npm run build --prefix frontend
```

完整媒体测试必须使用隔离实例. 测试会扫描媒体, 写入播放进度,
创建和释放会话, 并在传入 `--container` 时破坏指定测试缓存及重启该容器.
不要针对实际媒体库运行.

```bash
python3 backend/tests/playback_smoke.py \
  --fixtures /tmp/anime-playback-media \
  --font /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc

mkdir /tmp/anime-playback-cache

docker run -d --rm --name anime-server-test \
  -p 127.0.0.1:56081:8080 \
  -e ANIME_SERVER_CONFIG=/config/config.example.json \
  -v "$PWD/examples:/config:ro" \
  -v /tmp/anime-playback-media:/media:ro \
  -v /tmp/anime-playback-cache:/cache \
  anime-server:dev

curl --fail --retry 15 --retry-all-errors --retry-delay 1 http://127.0.0.1:56081/health

python3 backend/tests/playback_smoke.py \
  --url http://127.0.0.1:56081 \
  --container anime-server-test

docker stop anime-server-test
```

每次完整测试使用新的空缓存目录, 不复用旧测试的生成结果.
执行测试的用户需要 Docker 权限. 如果修改了测试实例的 `max_processes`,
用同值的 `--max-processes` 参数运行测试.

实际容量淘汰测试另建隔离实例, 使用独立的配置和空缓存目录,
把 `playback.cache_max_bytes` 设为 `1`, 再运行:

```bash
python3 backend/tests/playback_smoke.py \
  --url http://127.0.0.1:56082 \
  --container anime-server-eviction-test \
  --eviction-only
```

测试覆盖统一编码, 帧率上限, 实际音轨信号, 文本和位图字幕画面,
小数时间定位, 共享租约, 同键单任务, 并发限流, 无人观看后继续生成,
中间文件清理, 重启复用, 损坏缓存清理及进度持久化.
另检查实际转码中的正常停服和强制中断恢复, 确保未完成输出不被复用.
后端单元测试覆盖规范化缓存键, LRU 顺序, 活动和生成条目保护, 目录时间和启动恢复.
前端回归测试覆盖同租约跳转等待及改选目标, 暂停状态换轨和解码失败不自动发起新播放请求.
原生 iOS Safari 的最终兼容性仍需在真实设备上确认.

## 故障排查

| 现象                   | 检查方式                                                                                    |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| 启动后没有端口映射     | 检查 `docker compose config` 和 `docker compose port anime-server 8080`, 必要时重新创建容器 |
| Docker socket 权限不足 | 使用具备 Docker 权限的用户; 修改用户组后重新登录, 或按宿主机策略使用 sudo                   |
| 视频列表为空           | 检查 `/media` 挂载和读取权限, 然后点击 Scan videos                                          |
| 长时间等待名额         | 等待已有缓存生成完成, 或按机器能力调整并发数; 停止播放不取消生成                             |
| 恢复位置等待很久       | 新缓存从 0 连续生成, 等待生成范围覆盖目标; 可以拖回已生成位置                                |
| 首段生成很慢           | 检查 CPU, 字幕复杂度和字体大小; 首次使用新的轨道配置需要完整编码                            |
| 字幕没有字形           | 检查内嵌字体和镜像的 Noto CJK 字体; 查看播放器返回的 FFmpeg 错误                            |
| 探测失败被记住         | 点击 Retry probe, 或调用 `/probe`; 错误不会跨进程持久保存                                   |
| 会话不存在或过期       | 页面后台休眠可能停止保活; 手动点击 Play 或 Retry 创建同格式的新会话                         |
| 解码或 HLS 解析失败    | 查看播放器错误及容器日志. 不会自动请求其他编码或封装                                        |
| 缓存目录被占用         | 检查是否有另一实例挂载相同 `CACHE_DIR`, 为测试实例使用独立目录                              |
| 缓存超过配置大小       | 生成和活动条目受保护, 等待完成并释放租约; 容量上限不是硬配额                                |
| 磁盘已满               | 停止服务后清理 `/cache` 中指定的变体, 或增加空间; Stop 按钮不终止后台生成, 不要删除数据库  |
