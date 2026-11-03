# HTTP API

业务路由位于 `backend/src/api.rs`.
请求使用同源地址和 `Content-Type: application/json`.
当前没有用户鉴权, 不应直接暴露到公网.

构建使用媒体绝对路径和 PlaybackQuery, 不接受客户端指定内容标识或编码版本.
播放和管理使用服务端返回的稳定 handle_id, 它不是授权令牌.
旧 `/api/playback*`, `/api/files/*`, `/api/scan*`, `/status`, `/target` 和 `/heartbeat` 接口已经删除,
不提供兼容别名.

## 路由

| 方法   | 路径                                 | 作用                              |
| ------ | ------------------------------------ | --------------------------------- |
| GET    | `/api/config`                        | 读取配置                          |
| GET    | `/api/media`                         | 获取媒体 ID, 路径, 播放配置和剧集 ID |
| GET    | `/api/media/{file_id}/metadata`      | 探测单个媒体的实时 metadata       |
| PUT    | `/api/media/{file_id}/episode`       | 单独保存或清空共享剧集 ID         |
| POST   | `/api/media/identify`               | 按文件路径请求 DandanPlay 识别    |
| POST   | `/api/danmaku`                       | 按剧集读取或强制刷新弹幕文件缓存  |
| POST   | `/api/media/scan`                    | 全量扫描媒体目录                  |
| POST   | `/api/media/rescan`                  | 按路径重扫并返回轻量媒体条目      |
| PUT    | `/api/media/configs`                 | 按路径保存完整播放配置            |
| POST   | `/api/cache`                         | 使用 PlaybackQuery 构建或复用缓存 |
| GET    | `/api/cache`                         | 获取全部缓存及 worker 占用合计    |
| GET    | `/api/cache/{handle_id}`             | 获取单个缓存条目                  |
| POST   | `/api/cache/{handle_id}/retry`       | 解析源路径并重试或续接            |
| POST   | `/api/cache/{handle_id}/stop`        | 停止转码, 保留已发布前缀          |
| DELETE | `/api/cache/{handle_id}`             | 立即停止并删除缓存                |
| GET    | `/playback/{handle_id}/{asset_name}` | 获取已发布 HLS 资源               |
| GET    | `/health`                            | 检查 HTTP 服务存活                |

## 媒体列表

`GET /api/media` 返回 `200`:

```json
[
  {
    "file_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "path": "/media/series/episode.mkv",
    "configs": {
      "position": 125.5,
      "video_stream_index": 0,
      "audio_stream_index": null,
      "subtitle_stream_index": null
    },
    "episode_id": null
  }
]
```

列表只查询数据库, 不对媒体文件运行 ffprobe. 每个路径单独返回,
相同内容的路径使用相同 `file_id` 并共享 configs 和 episode_id.
episode_id 独立于播放配置, 不参与缓存身份.

展开或播放单个媒体时, 前端调用 `GET /api/media/{file_id}/metadata`.
成功返回 `200`:

```json
{
  "metadata": {
    "duration": 1400.0,
    "format": "matroska,webm",
    "video_streams": [
      {
        "stream_index": 0,
        "codec_name": "h264",
        "language": null,
        "title": null,
        "is_default": true,
        "is_forced": false
      }
    ],
    "audio_streams": [],
    "subtitle_streams": []
  },
  "configs": {
    "position": 125.5,
    "video_stream_index": 0,
    "audio_stream_index": null,
    "subtitle_stream_index": null
  }
}
```

后端按路径顺序探测该内容 ID 的第一个可访问副本. ID 不存在或所有副本都不可用时返回 `404`;
媒体不可读取或探测失败时返回 `422`.
流索引是 ffprobe 的容器全局索引, 不是数组下标.
视频封面和字体附件不作为可选流返回, 不提供外挂字幕.

## 扫描与配置

`POST /api/media/scan` 不需要请求体. 成功返回:

```json
{ "added": 3, "updated": 1, "removed": 2, "total": 40 }
```

全量扫描重新计算每个媒体的前 16 MiB MD5.
目录遍历或数据库操作失败会报错, 单文件哈希失败也会在处理后显式报错.
total 是扫描完成后数据库中的路径数量, 多副本分别计数.

`POST /api/media/rescan`:

```json
"/media/series/episode.mkv"
```

请求体是 JSON 路径字符串, 不使用 `{ "path": "..." }` 对象.
成功返回 `200` 和最新的轻量媒体条目:

```json
{
  "file_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "path": "/media/series/episode.mkv",
  "configs": {
    "position": 125.5,
    "video_stream_index": 0,
    "audio_stream_index": null,
    "subtitle_stream_index": null
  },
  "episode_id": null
}
```

文件已经消失时删除其路径关联并返回 JSON `null`.
路径必须位于媒体根目录, 不允许父目录跳转或指向目录外的符号链接.
重扫不停止缓存生成. 内容改变后, 旧缓存可能失去源路径, 不能再播放或重试.

`PUT /api/media/configs`:

```json
{
  "path": "/media/series/episode.mkv",
  "configs": {
    "position": 125.5,
    "video_stream_index": 0,
    "audio_stream_index": 1,
    "subtitle_stream_index": null
  }
}
```

请求体是 `{ "path": "...", "configs": { ... } }`.
成功返回 `204`, 未登记路径返回 `404`.
这是完整替换而不是 PATCH, 未提供的可选配置字段成为 null.
position 必须为 null 或有限非负秒数, 流索引必须为 null 或非负整数.
此接口不执行媒体探测, 不检查位置上界或索引是否实际存在,
实际构建时再对照实时 metadata 校验.

构建本身不保存偏好或观看进度, 由前端通过此接口显式保存.
配置共享于同内容的所有路径, 不随缓存停止或删除而删除.
此接口不修改 episode_id, 不能通过 configs 对象保存剧集关联.

`GET /api/config` 返回 `{"default_dark_mode":true}`, 不公开完整运行配置或 DandanPlay 凭据.

## 剧集 ID 与文件识别

`PUT /api/media/{file_id}/episode` 请求:

```json
{ "episode_id": 1234567 }
```

episode_id 必须为 `1..9007199254740991` 范围内的整数或显式 null.
字段不可省略, 不接受字符串, 小数或未知字段. 清空关联时发送 `{"episode_id":null}`.
成功返回 `200`:

```json
{
  "file_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "episode_id": 1234567
}
```

内容不存在或没有登记路径时返回 `404`. 手动保存不调用外部 API, 不探测媒体,
只更新同内容共享的 episode_id, 不改变播放进度或轨道偏好.

`POST /api/media/identify` 请求体是 JSON 路径字符串:

```json
"/media/series/episode.mkv"
```

后端先校验媒体目录边界并重扫内容身份. 每次请求都重新识别, 已有非空 ID 也不跳过上游.
读取文件名, 前 16 MiB MD5 和文件大小,
向 `https://api.dandanplay.net/api/v2/match` 发送 `hashAndFileName` 请求.
videoDuration 使用协议允许的 0, 不为识别运行 ffprobe.
使用服务端凭据生成 X-AppId, X-Timestamp 和 X-Signature, 不发送原始密钥,
媒体完整路径或文件内容.

`match_file` 返回 `Result<Option<u64>>`, 只有 `isMatched=true` 且恰好一个有效 ID 时返回 Some.
ID 必须为 `1..9007199254740991` 范围内的整数, 唯一精确结果覆盖已有或手动保存的 ID.
成功响应包含 `file_id`, 数据库最终的 `episode_id` 和本次唯一匹配标志 `matched`, 不返回候选列表.
例如唯一精确匹配:

```json
{
  "file_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "episode_id": 1234567,
  "matched": true
}
```

无匹配或模糊结果返回 `200` 和 `matched: false`, 即使模糊结果只有一项也不会保存.
此时保留数据库写回时的 ID, 包括识别期间并发手动保存的关联, 不自动清空.
例如未取得唯一匹配且原 ID 为 1234567:

```json
{
  "file_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "episode_id": 1234567,
  "matched": false
}
```

原关联为空时 episode_id 仍为 null. 前端以 matched 判断本次结果, 不能把非空 ID 当作匹配成功.
唯一匹配成功时则以识别结果覆盖写回时的 ID, 包括请求期间的手动修改.
用户仍可通过保存 ID 接口手动覆盖或清空关联.
上游返回后会再次扫描并核对路径内容和大小, 文件已消失或改变返回 `409`,
不把旧识别结果写入新内容.

可选凭据来自服务端环境变量 DANDAN_APP_ID 和 DANDAN_APP_SECRET,
Compose 从根目录 .env 传入, 不从 config.json 获取, 详见 [development.md](development.md).
未配置返回 `503`. 上游 HTTP, 业务和协议错误返回 `502`, 超时返回 `504`.
连接超时为 5 秒, 整个上游请求最多 20 秒, 响应正文限制为 1 MiB.
这些错误不等同于没有匹配, 失败时保留原关联并允许用户显式重试.

## 弹幕加载

`POST /api/danmaku` 请求:

```json
{ "episode_id": 1234567, "refresh": false }
```

episode_id 必须为 `1..9007199254740991` 范围内的整数, 不能为 null, 字符串或小数.
refresh 缺省为 false, 显式提供时必须为布尔值. 拒绝未知字段, 非法请求返回 `400`.
接口只根据剧集 ID 访问弹幕, 不查询或修改数据库, 不扫描媒体, 探测 metadata 或构建播放缓存.

成功返回 `200`:

```json
[
  {
    "time": 12.5,
    "mode": 1,
    "color": 16777215,
    "sender": "user-id",
    "comment_id": "9223372036854775807",
    "content": "example"
  }
]
```

响应直接使用弹幕数组, 空列表为 `[]`, 不包含 episode_id 或 comments 包装对象,
也不公开 `cached` 或缓存路径. episode_id 仅用于请求和前端当前媒体关联.
原始弹幕缓存为 `/data/ddp-danmaku/<episode_id>.json`,
保留上游 `{count, comments: [{cid, p, m}]}` JSON 的原始字节, 不写入规范化响应.
加载已有缓存时重新校验; 缓存缺失或 refresh=true 时下载, 校验成功后通过同目录临时文件原子替换.
缓存不设置 TTL, 不自动刷新. 同剧集请求串行化, 等待中的普通加载会重新检查缓存以避免重复下载.

规范化规则:

- p 使用 `时间,模式,颜色,发送者` 格式, time 必须有限且非负.
- mode 保留原始非负整数, 1 为滚动, 4 为底部, 5 为顶部, 未知模式不丢弃.
- color 为 `0..16777215` 的 RGB 整数, sender 为原始字符串, content 为 m 的纯文本.
- cid 为整数或字符串时无损转换为 comment_id 字符串, 缺失或 null 时返回 null.
- 返回数组按 time 稳定升序排序, 相同时间保持上游顺序, 不去重或过滤.
- 上游 comments 必须显式提供; 合法空数组返回空列表, 官方 `count:0, comments:null` 也视为空列表.
  其他 null, 缺失数组或无效条目报错, 条目错误带原始索引, 不用默认值伪造成功.

上游请求为 `GET https://api.dandanplay.net/api/v2/comment/{episode_id}?withRelated=true&chConvert=0`.
包含关联弹幕, 不转换简繁体. 签名只计算不含 query 的 API 路径, 复用环境变量凭据.
首次请求禁用自动重定向; 最多手动跟随 3 次 HTTPS 跳转, 不接受 URL 内嵌凭据,
后续请求不发送 X-AppId, X-Timestamp, X-Signature 或 Referer.
连接超时 5 秒, 下载和重定向整个链路最多 20 秒. 上游正文和单份本地缓存最多 32 MiB.

缓存命中不要求配置认证. 缓存缺失或强制刷新且未配置时返回 `503`.
上游 HTTP, 网络, 业务或解析错误返回 `502`, 超时返回 `504`.
本地读取, 缓存格式或持久化失败返回 `500`; 不存在之外的读取错误不会静默转为上游请求.
坏缓存可通过显式 Refresh 修复. 上游失败或新临时文件写入失败不替换旧缓存,
刷新失败不回退旧内容并伪装成功; 前端可保留自己已显示的列表同时显示错误.

## 构建缓存

默认创建只提交路径:

```bash
curl --fail http://localhost:56080/api/cache \
  -H 'Content-Type: application/json' \
  -d '{"path":"/media/series/episode.mkv"}'
```

也可以显式选择三类流:

```json
{
  "path": "/media/series/episode.mkv",
  "video_stream_index": 0,
  "audio_stream_index": 1,
  "subtitle_stream_index": null
}
```

视频字段缺省或 null 时使用保存的视频偏好, 没有偏好时选择首条视频流.
音频和字幕字段区分缺省与显式 null.
缺省先使用非空保存偏好, 音频再选择默认流或第一条流,
字幕再选择默认或强制字幕. 显式 null 始终关闭对应流.
没有适用的音频或字幕时关闭.
已保存的 null 不区分未选择和关闭, 因此前端恢复关闭偏好时显式发送 null.
非法索引返回 `400`, 不静默替换为其他流.

服务端重新扫描路径, 计算媒体身份并探测 metadata, 不依赖陈旧的客户端列表.
成功返回 `200` 和 `CacheBuild`:

```json
{
  "cache": {
    "handle_id": "9eb364b42c594741aa0c83a907d93d18",
    "spec": {
      "media_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "video_stream_index": 0,
      "audio_stream_index": 1,
      "subtitle_stream_index": null,
      "version": "h264-high-yuv420p-aac-fmp4-v2"
    },
    "phase": "loading",
    "buffered_until": 0.0,
    "error": null,
    "resources": { "encoder_slots": 0, "process_slots": 0, "ffmpeg_threads": 0 },
    "source_path": "/media/series/episode.mkv"
  },
  "media_meta": {
    "duration": 1400.0,
    "format": "matroska,webm",
    "video_streams": [],
    "audio_streams": [],
    "subtitle_streams": []
  },
  "media_configs": {
    "position": 125.5,
    "video_stream_index": 0,
    "audio_stream_index": 1,
    "subtitle_stream_index": null
  }
}
```

示例标识仅用于说明结构, 不代表该 spec 的实际哈希. 示例省略了实际流数组.
返回时不等待恢复或编码完成. 相同内容, 流选择和版本复用同一 handle_id,
源路径, 播放位置和线程预算不参与身份.
实际选轨结果只位于 cache.spec, 没有 session_id 或播放映射.

PlaybackQuery 不包含 position. 前端从 media.configs.position 或本地用户操作确定目标,
接近片尾的历史进度在前端从头播放. start_time, duration 和 selection
不再独立返回, duration 位于 media_meta 中.

## 缓存列表与状态

`GET /api/cache` 返回 `200`:

```json
{
  "entries": []
}
```

entries 按 handle_id 排序, 每条结构与构建响应的 cache 相同.
`GET /api/cache/{handle_id}` 直接返回单个 CacheSnapshot.
phase, buffered_until 和 error 均位于缓存对象顶层, 前后端需同步更新.
handle_id 必须为 32 个小写十六进制字符, 不接受目录路径或大小写别名.

source_path 由数据库按 media_id 查找, 选择路径排序后的第一个现存副本.
没有现存源路径时为 null, 即使 completed 仍不能播放或重试.
列表和详情不运行 ffprobe, 不隐式启动恢复或生成.

resources 只统计已经启动的转码 worker. 单个 worker 的 encoder_slots 和 process_slots 为 1,
ffmpeg_threads 为其实际持有的编码线程预算. 排队, 准备或退出后为 0.
这不是 CPU 使用率, 操作系统实际线程数量或浏览器解码开销,
不包含 ffprobe, 字幕准备和分片校验等短进程.
失败诊断保存在缓存对象的 error 字段, 其他状态为 null.

| phase     | 含义                                      |
| --------- | ----------------------------------------- |
| loading   | 正在恢复磁盘内容                          |
| ready     | 已恢复的未完成实例, 可启动生成            |
| running   | 正在等待预算或生成并验证分片              |
| stopping  | 正在取消并回收写入任务                    |
| completed | 已生成到媒体结束并验证                    |
| failed    | 恢复或生成失败, 显式构建或 retry 时可重试 |

buffered_until 是从零开始的连续发布前缀终点, 不是浏览器下载缓冲.
只有 `position < buffered_until` 才能播放, 等于终点时仍需等待.
前端 CacheStore 合并并发查询. 等待目标时约每 500 ms 刷新,
存在活动任务时约每秒刷新, 全部稳定后约每 5 秒同步其他客户端的操作.
没有播放心跳或服务端目标位置.
180 秒没有阶段或生成进度变化时报告等待失败, 不是固定总等待时限.

已验证前缀在后续生成失败后仍可播放.
failed 且目标未覆盖时前端明确报错, 由用户手动重试, 不自动构建.

## 重试, 停止与删除

`POST /api/cache/{handle_id}/retry` 不需要请求体, 返回 `200` 和 CacheBuild.
按 media_id 选择源路径, 重新扫描内容身份并校验三类流索引,
然后复用同一 handle. 源文件内容改变返回 `422`, 不删除原缓存或创建新句柄.
已有 completed 或 running 缓存不重复转码. 并发删除后返回 `404`, 不隐式重建.

`POST /api/cache/{handle_id}/stop` 不需要请求体, 返回 `200` 和 CacheSnapshot.
Running 转为 Stopping, 等待 worker 回收后返回 Ready, 保留已发布前缀.
若最后的清单已完整提交则为 Completed. 其他非活动阶段保持当前状态.
返回时被停止 worker 的资源计数为 0, 后续通过 retry 续接.

`DELETE /api/cache/{handle_id}` 成功返回 `204`, 不存在返回 `404`.
立即停止转码, 删除整个缓存实例目录并移除登记.
不保护任何浏览器当前的播放引用, 后续资源请求返回 `404`.
HTTP 请求取消不会中断已经开始的 stop 或 delete 清理.
缓存实例持久保留, 服务关闭, 重启, 无源或生成失败不会自动删除实例.
关闭时回收正在运行的 worker, 已发布前缀留待重启后显式续接.
播放器本地 Stop, 换片, 解码失败或页面关闭均不调用这两个管理接口.

## 播放资源

`GET /playback/{handle_id}/{asset_name}`:

| 资源                              | Content-Type                  |
| --------------------------------- | ----------------------------- |
| index.m3u8                        | application/vnd.apple.mpegurl |
| init.mp4                          | video/mp4                     |
| segment_000000.m4s 等六位编号分片 | video/iso.segment             |

至少发布一个有效分片后才开放清单和 init.
资源必须存在于正式清单, 不可读取 instance.json, complete 或私有工作文件.
支持 Range, 成功返回 `200` 或 `206`.
响应设置 `Cache-Control: no-store` 和 `X-Content-Type-Options: nosniff`.
播放请求不接收 PlaybackQuery, 不扫描或启动转码.
源路径不可用时返回 `404`, 不提供无源缓存直放.
浏览器请求 `/playback/{handle_id}/index.m3u8` 后,
清单中的相对 init 和分片地址自然继续使用同一 handle_id.

## 错误与健康检查

业务错误记录日志并返回 `{"error":"具体原因"}`.
异步生成失败通过缓存对象顶层的 phase 和 error 表达, 服务日志保留详细诊断.

| 状态码 | 主要情况                                                    |
| ------ | ----------------------------------------------------------- |
| 400    | JSON 或字段无效, 路径形状不合法, 流选择不存在, 配置数值非法 |
| 404    | 源路径不可用, handle 不存在, 资源尚不可用或禁止访问         |
| 409    | 句柄正在停止, 或源文件在识别期间发生变化                    |
| 422    | 媒体无法读取或探测, 单路径扫描失败, 重试时内容身份已改变    |
| 500    | 数据库, 全量扫描, 缓存或其他内部操作失败                    |
| 502    | DandanPlay 网络, HTTP, 业务状态或响应协议错误               |
| 503    | 尚未配置 DandanPlay 应用凭据                               |
| 504    | DandanPlay 识别或弹幕下载请求超时                          |

`GET /health` 返回 `{"status":"ok"}`, 不扫描或探测媒体.
其他路径交给 `/app/web` 的静态文件服务.
