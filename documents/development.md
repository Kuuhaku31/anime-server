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
后端入口是 `backend/src/main.rs`. `api.rs`, `db.rs`, `tasks.rs`, `video.rs`, `cache.rs`
声明各自同名目录下的子模块, 不使用 `mod.rs`.
数据库初始化文件是 `backend/sql/init.sql`.

Rust 类型, 字段, 函数和模块默认私有. 仅供 `video` 内部共享的接口使用
`pub(super)`, 必须跨模块使用的接口最高为 `pub(crate)`, 不使用无限制的 `pub`.
Serde 序列化和反序列化不依赖字段公开, 不为接口响应字段额外扩大可见性.

`video.rs` 是媒体执行模块的统一接口入口, 通过重导出提供执行参数,
分片生成与校验, 进程预算, 会话响应及管理器. 外部代码只使用 `crate::video::{...}`, 不导入其私有子模块.
类型和方法实现保留在子模块中, `video.rs` 的注释集中说明调用链及接口职责.
随机名称和媒体样本辅助接口仅在测试构建中重导出.

`cache.rs` 是 crate 层的缓存入口, 提供 CacheManager, FolderState 只读快照和 CachePin.
活动缓存共享, 生成生命周期和淘汰编排由入口管理. 私有 `cache/metadata.rs`
负责 ffprobe 探测, 成功元数据缓存及过期清理; 私有 `cache/disk.rs`
负责磁盘占用统计和目录访问时间持久化. CacheManager 通过 `crate::video`
调用媒体执行能力, 不暴露或依赖 video 私有子模块路径.
`sessions.rs` 保留在 crate 层, 直接使用 `crate::cache` 管理共享缓存租约.
video 下的编码, 资源, 分片, 进程和执行参数文件保持原位.

播放请求 `PlaybackRequest` 和创建响应 `PlaybackStart` 定义在 `models.rs`,
由 `video.rs` 保留重导出入口. 请求选轨的缺省/null 区分及响应 flatten 结构不变.

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
缓存根目录使用独占锁, 第二个实例会明确报错. 服务启动时校验并修复完整或部分缓存,
清理无效尾部及遗留中间文件, 不恢复客户端会话或自动启动编码.

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
    "max_variant_threads": 8,
    "max_total_ffmpeg_threads": 16,
    "idle_timeout_seconds": 60,
    "startup_timeout_seconds": 90
  },
  "dandanplay": {
    "enabled": false,
    "api_base": ""
  }
}
```

| 播放配置                   | 说明                                                                                           |
| -------------------------- | ---------------------------------------------------------------------------------------------- |
| `cache_max_bytes`          | 缓存容量字节数, 正整数 `u64`, 默认 `10737418240`, 即 10 GiB                                    |
| `max_processes`            | 全部媒体进程上限, 范围 `2..16`, 默认 2. 编码器最多占用 `max_processes - 1`, 为解码校验保留名额 |
| `ffmpeg_threads`           | 编码器启动需要的基础线程数, 默认 2, 必须不大于单变体上限                                       |
| `max_variant_threads`      | 单个编码器线程上限, 默认 8, 最大 32                                                            |
| `max_total_ffmpeg_threads` | 全部 H.264 编码器的加权线程预算, 默认 16, 范围为基础线程数到 256                               |
| `idle_timeout_seconds`     | 无保活请求后的会话过期时间, 范围 `20..3600` 秒                                                 |
| `startup_timeout_seconds`  | 字幕准备及首段生成各阶段的超时, 范围 `10..600` 秒                                              |

缺少 `playback` 时使用上述默认值, 未知的播放配置字段会报错.
修改应用配置后重启容器. DandanPlay 当前未实现真实 API 调用.
Web 界面在缓存状态及生成范围连续 180 秒没有进展时显示错误.
生成正常推进时允许继续等待较远的目标位置, 不设置 180 秒总时限.
HLS 元数据加载最多等待 30 秒, 超时后显示错误并释放租约.
最后一个租约释放时, 编码器最多再提交一个完整分片, 30 秒内仍未完成则强制停止.

任务启动前, 如果任一客户端目标尚未覆盖, 非阻塞申请最多 `max_variant_threads` 个线程;
否则只申请基础线程. 预算不足时使用当前可取得且不低于基础值的数量启动.
运行中的任务不热调, 不抢占其他任务的线程, 实际编码进程退出时归还许可.
这个预算不包括解码器, 滤镜和工具内部辅助线程, 不是操作系统线程总数上限.
默认 2 个进程名额仅容纳一个编码器加一个校验进程; 需要两个编码器时将 `max_processes` 设为 3.

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

| 项目         | 固定规则                                                                               |
| ------------ | -------------------------------------------------------------------------------------- |
| 视频         | libx264, High, 8-bit `yuv420p`, CRF 23, veryfast                                       |
| 分辨率与帧率 | 保持比例, 不放大小视频, 最高 1920x1080; 按源声明帧率与 60 的较小值进行有理数恒帧率采样 |
| HDR          | 转为 BT.709 SDR                                                                        |
| 音频         | 选定一轨, AAC-LC, 双声道, 48 kHz, 192 kbps; 可选择无音轨                               |
| 字幕         | 选定一轨服务端烧录, 或关闭字幕                                                         |
| 传输         | fMP4 HLS, 目标分段长度 4 秒, event playlist                                            |
| 浏览器       | Safari 原生 HLS, 其他支持 MSE 的浏览器使用 HLS.js                                      |

传输适配在加载前选定. Chromium 即使声明可能支持原生 HLS, 也优先使用 HLS.js,
避免将仍在生成的 fMP4 event playlist 交给不完整的原生实现.

只支持容器内字幕轨, 索引使用 ffprobe 的真实流索引. 不扫描或加载外挂字幕.
ASS/SSA 和其他文本字幕先提取到 `/cache/subtitles/<random-id>.ass`, 再由 libass 渲染.
内嵌字体提取到 `/cache/fonts/<generation-id>/`, 文件名独立随机生成;
缺少字体时使用运行镜像的回退字体. 位图字幕直接通过 overlay 烧录, 不提取 ASS.
首次生成从时间 0 开始. 续接只编码未缓存的视频尾部, 但音频重采样和位图字幕状态依赖历史:
第二路输入从源头读取音频和位图字幕, 不重新编码已有视频.
文本字幕在原时间轴上烧录, 再归一化容器偏移和视频采样时钟.
VFR 输入按同一恒帧率规则采样, 必要的重复或丢帧来自固定输出规则, 不是续接边界丢失.
生成结束, 失败或停止后立即删除中间字幕和字体.

切换音轨, 字幕或字幕关闭时:

1. 保存原始视频时间及暂停/播放状态.
2. 停止浏览器加载并释放旧会话.
3. 以选定轨道和原始时间创建新租约, 复用或开始生成对应缓存.
4. 当目标时间严格小于 `buffered_until` 时附加新流, 不额外等待 0.5 秒.
5. 原先正在播放时继续播放, 原先暂停时保持暂停.

服务端不按用户跳转位置创建新编码任务. 仅在恢复部分缓存时 seek 到已验证尾部附近,
精确裁切并继续顺序生成.
所有缓存的 `start_time` 固定为 0, 浏览器直接定位原视频时间,
实际画面仍受源视频帧间隔约束.
已生成范围内的 seek 可以直接使用当前流. 超出范围时保持同一租约,
等待顺序生成覆盖目标位置后重新加载 HLS. 等待时仍可拖动到另一个位置或停止播放.
浏览器阻止自动播放时会显示 `Press Play to start`, 需要用户点击播放.

浏览器解码错误, HLS 致命错误或分段解析错误会直接显示, 停止加载并释放会话.
不会请求另一种格式, 也不会自动重建播放会话.
用户点击 Retry 才重新请求同样的输出. 普通网络分片重试不改变格式.

## HTTP API

全部路由, 请求与响应示例, 会话生命周期和错误码见 [HTTP API 文档](api.md).

## 数据与资源管理

`/data/main.db` 只保存业务数据和进度. CacheManager 维护按文件 MD5 索引的媒体元数据 Map,
只缓存探测成功结果, 最多 1000 项且一小时未访问后过期. 缓存未命中时由 CacheManager
查询数据库中的源文件路径并运行 ffprobe; 显式 probe 会跳过已有结果, 探测失败不会缓存.
服务重启后元数据重新探测.

播放请求和媒体文件路径由不同边界管理. API 负责将数据库中的轨道偏好和观看进度合并到
`PlaybackRequest`; SessionManager 按文件 MD5 向 CacheManager 临时取得 `MediaInfo`,
构造不可变的 `PlaybackVariantConfig` 并计算 variant-key. CacheManager 接收该配置,
自行查询源路径并组装编码参数, 活动缓存 Map 仅在相应 variant 被请求时创建条目.
SessionManager 不保存媒体元数据或文件路径, API 也不直接操作元数据缓存或活动缓存.

Rust `PlaybackVariantConfig` 依次包含 `src_file_pre16mib_md5`, `video_stream`,
`audio_stream`, `subtitle_stream`, `encoder_profile`.
使用 `serde_json::to_vec` 按字段顺序生成紧凑 JSON, 保留空轨道的 `null`,
计算 MD5 后得到 32 位小写 `variant-key`. 固定编码配置为 `h264-high-yuv420p-aac-fmp4-v2`.
v2 固定无 B 帧, 视频时间基 90000, 音频时间基 48000, AAC 预热处理和有理数采样时钟,
不能与旧 v1 缓存混用. 动态线程数不是配置字段, 同一配置的不同线程任务仍使用兼容的 init.
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
运行时 `HashMap<variant-key, Arc<CacheFolder>>` 只保存活动元素.
SessionManager 保存会话到 variant-key 的关联, 各会话目标及保活时间, 并将同一变体的最大目标
汇总到缓存文件夹. 最后一个会话离开后, 缓存生成停止; worker 收尾且媒体文件打开保护引用释放后,
维护任务才从活动 Map 移除该文件夹.
查询播放状态时, CacheManager 返回字段私有的 FolderState 克隆快照, SessionManager 通过只读 getter
读取状态并构造 SessionStatus. 媒体时长由 CacheManager 独立读取, 不重复保存在 FolderState 中.
状态和媒体请求必须通过 session 及活动集合, 不支持按客户端提供的缓存键访问磁盘.

最后一个租约离开时, 排队和准备任务立即取消; 编码任务记录当前已提交数量,
最多再验证和发布一个分片后停止. 30 秒未等到新分片则强制终止并回收进程.
停止期间新租约可撤销尚未结束的停止请求; 如果已经开始最终清理, 则等待旧写入者退出.
DELETE 不等待此过程. 服务正常关闭采用相同停止规则, Compose 的宽限期为 40 秒.

不保存 `metadata.json` 或单独的持久缓存索引.
FFmpeg 写入变体下的私有 `.work-<id>` 目录. 首次发布正式 init 后不再替换它.
Rust 比较新旧 init 的轨道, timescale, AVC/AAC 描述和默认值, 去除可选 `sidx`,
修正 `mfhd` 序号与逐轨 `tfdt`, 保持媒体负载不变.
每段都与正式 init 执行实际解码, 通过后原子发布分片和 playlist.
续接只追加已验证尾部, 不覆盖已有 init 或分片.

音频从固定原点重采样, 仅编码少量预热音频和新尾部, 丢弃负时间的 AAC 预热包.
读取旧音频或字幕历史有额外成本, 但不重编码已缓存的视频.
任务正常到 EOF 并验证全部输出后原子写入 ENDLIST 和空 `complete` 标记,
然后清理中间文件. 标记只表示媒体完整, 不表示后台清理已经结束.

重启校验所有连续分片的 MP4 结构, 样本范围, 序号和逐轨时间轴, 并实际解码首段及尾段.
损坏尾部从末端截断, 保留有效前缀; 已发布的中间段不会在每次恢复时全部重新解码.
崩溃前已发布但尚未写入 playlist 的有效分片可以找回, 已写 ENDLIST 而未写标记时也可恢复完成状态.
未完成缓存保留没有 ENDLIST 的 event playlist, 启动时不创建活动元素或自动续接.
真正的恢复, 编码和清理错误均明确记录, 正常取消不会掩盖其他错误.
启动清理仅针对服务生成的命名, 不删除未知名称的文件.
只保留 FFmpeg 最近 4000 字节 stderr 作为错误详情, 不写持久转码日志.

容量统计包含 HLS 和字幕, 字体中间文件的实际字节数.
最后使用时间记录在内存中, 并通过变体目录修改时间跨重启保留;
每 5 秒维护一次, 释放和生成完成时也执行维护, 避免每个分片请求都写盘.
超过 `cache_max_bytes` 时, 按 LRU 删除活动集合外的完整或部分变体,
直到满足上限或没有可淘汰条目. 文件打开期间持有的旧元素引用同样受到保护.
淘汰先在锁内原子改名, 再在锁外删除, 不长时间阻塞其他缓存的状态请求.
停止清理中的变体仍受保护, 允许暂时超限; 超大单个变体在退出活动集合后也会被删除.
不要在服务运行时手动删除缓存, 修改完成标记或接管中间文件.
备份数据库前建议停止容器, 并保留整个数据目录中的 SQLite 数据及可能存在的 WAL 文件.

## 验证

```bash
cargo test --locked -p anime-server
npm test --prefix frontend
npm run build --prefix frontend
```

完整媒体测试必须使用隔离实例. 测试会扫描媒体, 写入播放进度,
创建和释放会话, 并在传入 `--container` 时暂停或终止测试编码器, 破坏测试缓存及重启容器.
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
用同值的 `--max-processes` 参数运行测试. 全局线程预算和空闲时间有修改时,
同时传入对应的 `--thread-budget` 和 `--idle-timeout`.
验证两编码器预算分配可使用 `max_processes=3`, `max_total_ffmpeg_threads=10`,
配合 `--max-processes 3 --thread-budget 10`.

实际容量淘汰测试另建隔离实例, 使用独立的配置和空缓存目录,
把 `playback.cache_max_bytes` 设为 `1`, 再运行:

```bash
python3 backend/tests/playback_smoke.py \
  --url http://127.0.0.1:56082 \
  --container anime-server-eviction-test \
  --eviction-only
```

测试覆盖统一编码, 帧率上限, 实际音轨信号, 文本和位图字幕画面,
小数时间定位, 服务端进度与选轨, 共享租约, 同键单任务, 加权线程预算,
排队取消, 分片边界停止, 新租约撤销停止, 30 秒强制回收和会话过期.
续接检查原前缀的 inode 和摘要不变, 全部帧和音频样本数一致, 时间戳误差小于 1 毫秒,
逐帧图像与逐窗口波形满足误差阈值, 包括文本字幕, 位图字幕, 44.1 kHz 重采样及 VFR 输入.
另检查完整复用, 正常和强制重启, 损坏尾部修复, 部分缓存 LRU 和进度持久化.
前端回归测试覆盖同租约跳转等待及改选目标, 暂停状态换轨和解码失败不自动发起新播放请求.
原生 iOS Safari 的最终兼容性仍需在真实设备上确认.

## 故障排查

| 现象                   | 检查方式                                                                                    |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| 启动后没有端口映射     | 检查 `docker compose config` 和 `docker compose port anime-server 8080`, 必要时重新创建容器 |
| Docker socket 权限不足 | 使用具备 Docker 权限的用户; 修改用户组后重新登录, 或按宿主机策略使用 sudo                   |
| 视频列表为空           | 检查 `/media` 挂载和读取权限, 然后点击 Scan videos                                          |
| 长时间等待名额         | 检查进程名额和全局线程预算; 默认仅一个编码器, 需要两个时将 `max_processes` 设为 3           |
| 恢复位置等待很久       | 新缓存从 0 连续生成, 等待生成范围覆盖目标; 可以拖回已生成位置                               |
| 首段或续接启动很慢     | 检查 CPU, 字幕和字体; 首次配置从 0 编码, 续接仍需读取音频及字幕历史                         |
| 字幕没有字形           | 检查内嵌字体和镜像的 Noto CJK 字体; 查看播放器返回的 FFmpeg 错误                            |
| 探测失败被记住         | 点击 Retry probe, 或调用 `/probe`; 错误不会跨进程持久保存                                   |
| 会话不存在或过期       | 页面后台休眠可能停止保活; 手动点击 Play 或 Retry 创建同格式的新会话                         |
| 解码或 HLS 解析失败    | 查看播放器错误及容器日志. 不会自动请求其他编码或封装                                        |
| 缓存目录被占用         | 检查是否有另一实例挂载相同 `CACHE_DIR`, 为测试实例使用独立目录                              |
| 缓存超过配置大小       | 活动目录及媒体打开引用受保护, 等待最后租约释放和后台停止; 容量上限不是硬配额                |
| 磁盘已满               | 停止服务后清理 `/cache` 中指定的变体或增加空间, 不要删除数据库                              |


## 容器运行时目录结构

```
/
├── app/
│   ├── anime-server           # Rust 后端可执行文件
│   └── web/                   # npm run build 输出的 frontend/dist
│       ├── index.html
│       ├── assets/
│       │   ├── *.js
│       │   └── *.css
│       └── licenses/
│           └── hls.txt
├── config/                    # 宿主机 CONFIG_DIR, 只读挂载
│   └── config.json
├── media/                     # 宿主机 MEDIA_DIR, 只读挂载
│   └── ...
├── data/                      # 宿主机 DATA_DIR, 读写挂载
│   ├── main.db
│   ├── ddp-ani/
│   └── ddp-danmaku/
├── cache/                     # 宿主机 CACHE_DIR, 读写挂载
│   ├── playback/
│   │   └── <variant-key>/
│   │       ├── complete       # 仅完整缓存存在此标记
│   │       ├── index.m3u8
│   │       ├── init.mp4
│   │       └── segment_*.m4s
│   ├── subtitles/             # 转码期间的临时字幕文件
│   └── fonts/                 # 转码期间提取的字体附件
└── usr/
    ├── bin/                   # Debian 和 FFmpeg 等运行工具
    └── share/fonts/           # 中文回退字体
```