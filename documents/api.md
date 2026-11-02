# HTTP API

业务路由位于 `backend/src/api.rs` 和 `backend/src/api/playback.rs`.
请求使用同源地址和 `Content-Type: application/json`.
当前没有用户鉴权, 不应直接暴露到公网.

构建使用媒体绝对路径和 PlaybackQuery, 不接受客户端指定内容标识或编码版本.
播放和管理使用服务端返回的稳定 handle_id, 它不是授权令牌.
旧 `/api/playback*`, `/api/files/*`, `/api/scan*`, `/status`, `/target` 和 `/heartbeat` 接口已经删除,
不提供兼容别名.

## 路由

| 方法   | 路径                                  | 作用                       |
| ------ | ------------------------------------- | -------------------------- |
| GET    | `/api/config`                         | 读取前端默认主题           |
| GET    | `/api/media`                          | 获取 `[path, MediaInfo][]` |
| POST   | `/api/media/scan`                     | 全量扫描媒体目录           |
| POST   | `/api/media/rescan`                   | 按路径重扫并重新探测单文件 |
| PUT    | `/api/media/configs`                  | 按路径保存完整播放配置     |
| POST   | `/api/cache`                          | 使用 PlaybackQuery 构建或复用缓存 |
| GET    | `/api/cache`                          | 获取全部缓存及 worker 占用合计 |
| GET    | `/api/cache/{handle_id}`              | 获取单个缓存条目 |
| POST   | `/api/cache/{handle_id}/retry`        | 解析源路径并重试或续接 |
| POST   | `/api/cache/{handle_id}/stop`         | 停止转码, 保留已发布前缀 |
| DELETE | `/api/cache/{handle_id}`              | 立即停止并删除缓存 |
| GET    | `/playback/{handle_id}/{asset_name}`  | 获取已发布 HLS 资源 |
| GET    | `/health`                             | 检查 HTTP 服务存活         |

## 媒体列表

`GET /api/media` 返回 `200`:

```json
[
  [
    "/media/series/episode.mkv",
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
  ]
]
```

每个路径单独展示, 相同内容共享 configs.
metadata 每次按路径实时探测, 任一文件探测失败返回错误,
不伪造空元数据或静默省略该项.
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
{ "path": "/media/series/episode.mkv" }
```

成功返回 `200` 和最新的 `[path, MediaInfo]`.
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

成功返回 `204`, 未登记路径返回 `404`.
这是完整替换而不是 PATCH, 未提供的可选配置字段成为 null.
position 必须为 null 或有限非负秒数, 流索引必须为 null 或非负整数.
此接口不执行媒体探测, 不检查位置上界或索引是否实际存在,
实际构建时再对照实时 metadata 校验.

构建本身不保存偏好或观看进度, 由前端通过此接口显式保存.
配置共享于同内容的所有路径, 不随缓存停止或删除而删除.

`GET /api/config` 返回 `{"default_dark_mode":true}`, 不公开完整运行配置.

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
    "status": {
      "phase": "loading",
      "buffered_until": 0.0,
      "resources": { "encoder_slots": 0, "process_slots": 0, "ffmpeg_threads": 0 },
      "error": null
    },
    "source_path": "/media/series/episode.mkv"
  },
  "media": {
    "metadata": {
      "duration": 1400.0,
      "format": "matroska,webm",
      "video_streams": [],
      "audio_streams": [],
      "subtitle_streams": []
    },
    "configs": {
      "position": 125.5,
      "video_stream_index": 0,
      "audio_stream_index": 1,
      "subtitle_stream_index": null
    }
  }
}
```

示例标识仅用于说明结构, 不代表该 spec 的实际哈希. 示例省略了实际流数组.
返回时不等待恢复或编码完成. 相同内容, 流选择和版本复用同一 handle_id,
源路径, 播放位置和线程预算不参与身份.
实际选轨结果只位于 cache.spec, 没有 session_id 或播放映射.

PlaybackQuery 不包含 position. 前端从 media.configs.position 或本地用户操作确定目标,
接近片尾的历史进度在前端从头播放. start_time, duration 和 selection
不再独立返回, duration 位于 media.metadata 中.

## 缓存列表与状态

`GET /api/cache` 返回 `200`:

```json
{
  "entries": [],
  "resources": { "encoder_slots": 0, "process_slots": 0, "ffmpeg_threads": 0 }
}
```

entries 按 handle_id 排序, 每条结构与构建响应的 cache 相同.
resources 是本轮全部条目的转码 worker 占用合计.
`GET /api/cache/{handle_id}` 直接返回单个 CacheEntry.
handle_id 必须为 32 个小写十六进制字符, 不接受目录路径或大小写别名.

source_path 由数据库按 media_id 查找, 选择路径排序后的第一个现存副本.
没有现存源路径时为 null, 即使 completed 仍不能播放或重试.
列表和详情不运行 ffprobe, 不隐式启动恢复或生成.

status.resources 只统计已经启动的转码 worker. 单个 worker 的 encoder_slots 和 process_slots 为 1,
ffmpeg_threads 为其实际持有的编码线程预算. 排队, 准备或退出后为 0.
这不是 CPU 使用率, 操作系统实际线程数量或浏览器解码开销,
不包含 ffprobe, 字幕准备和分片校验等短进程.
失败诊断保存在 status.error, 其他状态为 null.

| phase     | 含义                                 |
| --------- | ------------------------------------ |
| loading   | 正在恢复磁盘内容                     |
| ready     | 已恢复的未完成实例, 可启动生成       |
| running   | 正在等待预算或生成并验证分片         |
| stopping  | 正在取消并回收写入任务               |
| completed | 已生成到媒体结束并验证               |
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

`POST /api/cache/{handle_id}/stop` 不需要请求体, 返回 `200` 和 CacheEntry.
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
异步生成失败通过 status.phase 和 status.error 表达, 服务日志保留详细诊断.

| 状态码 | 主要情况                                                       |
| ------ | -------------------------------------------------------------- |
| 400    | JSON 或字段无效, 路径形状不合法, 流选择不存在, 配置数值非法    |
| 404    | 源路径不可用, handle 不存在, 资源尚不可用或禁止访问 |
| 409    | 句柄正在停止, 不能同时接受新的构建或重试 |
| 422    | 媒体无法读取或探测, 单路径扫描失败, 重试时内容身份已改变 |
| 500    | 数据库, 全量扫描, 缓存或其他内部操作失败                       |

`GET /health` 返回 `{"status":"ok"}`, 不扫描或探测媒体.
其他路径交给 `/app/web` 的静态文件服务.
