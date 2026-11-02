# HTTP API

业务路由位于 `backend/src/api.rs` 和 `backend/src/api/playback.rs`.
请求使用同源地址和 `Content-Type: application/json`.
当前没有用户鉴权, 不应直接暴露到公网.

本次契约使用媒体绝对路径, 不接受客户端提供的媒体内容标识, 缓存键或编码版本.
旧 `/api/files/*`, `/api/scan*`, `/status`, `/target` 和 `/heartbeat` 接口已经删除,
不提供兼容别名.

## 路由

| 方法   | 路径                                  | 作用                       |
| ------ | ------------------------------------- | -------------------------- |
| GET    | `/api/config`                         | 读取前端默认主题           |
| GET    | `/api/media`                          | 获取 `[path, MediaInfo][]` |
| POST   | `/api/media/scan`                     | 全量扫描媒体目录           |
| POST   | `/api/media/rescan`                   | 按路径重扫并重新探测单文件 |
| PUT    | `/api/media/configs`                  | 按路径保存完整播放配置     |
| POST   | `/api/playback`                       | 创建最小播放会话           |
| GET    | `/api/playback/{session_id}`          | 查询缓存快照               |
| DELETE | `/api/playback/{session_id}`          | 幂等释放会话映射           |
| GET    | `/playback/{session_id}/{asset_name}` | 获取已发布 HLS 资源        |
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
重扫不终止已创建的播放会话.

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
实际播放时再对照实时 metadata 校验.

创建播放本身不保存偏好或观看进度, 由前端通过此接口显式保存.
配置共享于同内容的所有路径, 不随 session 释放而删除.

`GET /api/config` 返回 `{"default_dark_mode":true}`, 不公开完整运行配置.

## 创建播放

默认创建只提交路径:

```bash
curl --fail http://localhost:56080/api/playback \
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

服务端在播放前重新计算媒体身份并探测 metadata, 不依赖陈旧的客户端列表.
成功返回 `200`:

```json
{
  "session_id": "9eb364b42c594741aa0c83a907d93d18",
  "url": "/playback/9eb364b42c594741aa0c83a907d93d18/index.m3u8",
  "cache": { "phase": "loading", "buffered_until": 0.0 },
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
  },
  "video_stream_index": 0,
  "audio_stream_index": 1,
  "subtitle_stream_index": null
}
```

示例省略了 metadata 中的实际流数组内容.
返回时缓存可能为 loading, ready, running 或 completed,
不等待恢复或编码完成. session_id 是不透明映射, 与媒体和缓存标识无关.
不同 session 可以共享同一个缓存实例, 每个会话仍有独立 URL.

PlaybackQuery 不包含 position. 前端从 media.configs.position 或本地用户操作确定目标,
接近片尾的历史进度在前端从头播放. start_time, duration 和 selection
不再作为旧会话状态字段返回, duration 位于 media.metadata 中.

## 状态与等待

`GET /api/playback/{session_id}` 返回 `200`:

```json
{ "phase": "running", "buffered_until": 128.0 }
```

只返回上述两个字段, 不包含媒体目录, 缓存键, session_id, 目标位置或心跳时间.

| phase     | 含义                                 |
| --------- | ------------------------------------ |
| loading   | 正在恢复磁盘内容                     |
| ready     | 已恢复的未完成实例, 可启动生成       |
| running   | 正在等待预算或生成并验证分片         |
| stopping  | 正在取消并回收写入任务               |
| completed | 已生成到媒体结束并验证               |
| failed    | 恢复或生成失败, 下次创建播放时可重试 |

buffered_until 是从零开始的连续发布前缀终点, 不是浏览器下载缓冲.
只有 `position < buffered_until` 才能播放, 等于终点时仍需等待.
前端等待期间约每 500 ms 查询, 正常未完成阶段约每秒刷新,
completed 不需要心跳或持续轮询.
180 秒没有阶段或生成进度变化时报告等待失败, 不是固定总等待时限.

已验证前缀在后续生成失败后仍可播放.
failed 且目标未覆盖时前端立即显示错误并释放映射,
由用户手动重试, 不自动创建新 session.

## 释放与媒体资源

`DELETE /api/playback/{session_id}` 幂等返回 `204`.
只删除映射, 不停止共享缓存生成, 不清理磁盘缓存或观看记录.
会话无空闲过期机制, 未送达 DELETE 的映射保留到显式释放或服务重启.

`GET /playback/{session_id}/{asset_name}`:

| 资源                              | Content-Type                  |
| --------------------------------- | ----------------------------- |
| index.m3u8                        | application/vnd.apple.mpegurl |
| init.mp4                          | video/mp4                     |
| segment_000000.m4s 等六位编号分片 | video/iso.segment             |

至少发布一个有效分片后才开放清单和 init.
资源必须存在于正式清单, 不可读取 instance.json, complete 或私有工作文件.
支持 Range, 成功返回 `200` 或 `206`.
响应设置 `Cache-Control: no-store` 和 `X-Content-Type-Options: nosniff`.
释放后的 session 不能读取状态或资源.

## 错误与健康检查

业务错误记录日志并返回 `{"error":"具体原因"}`.
异步生成失败通过 phase 表达, 详细 FFmpeg 诊断位于服务日志.

| 状态码 | 主要情况                                                       |
| ------ | -------------------------------------------------------------- |
| 400    | JSON 或字段无效, 路径形状不合法, 流选择不存在, 配置数值非法    |
| 404    | 媒体路径未登记或已消失, session 不存在, 资源尚不可用或禁止访问 |
| 422    | 媒体无法读取或探测, 单路径扫描失败                             |
| 500    | 数据库, 全量扫描, 缓存或其他内部操作失败                       |

`GET /health` 返回 `{"status":"ok"}`, 不扫描或探测媒体.
其他路径交给 `/app/web` 的静态文件服务.
