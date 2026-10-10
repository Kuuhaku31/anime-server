# HTTP API

业务路由定义在 `backend/src/api.rs`, 播放路由定义在 `backend/src/api/playback.rs`.
主路由通过 `.merge(playback::router())` 合并播放接口, `.with_state(state)` 注入共享应用状态.
注册路由不会执行扫描或创建播放任务.

本文示例使用 `http://localhost:56080`, 实际地址取决于宿主机端口映射.
接口没有用户鉴权, 不应直接暴露到公网. JSON 请求使用 `Content-Type: application/json`.

- `{file_id}` 是源文件前 16 MiB 的 MD5, 对应代码中的 `{pre16mib_md5}` 或文件接口中的 `{id}`.
- `{session_id}` 是创建播放会话时返回的不透明 ID, 与文件 ID 和缓存键不同.
- 播放配置和缓存键由服务端生成, 客户端不能提供媒体路径或直接访问缓存目录.

## 路由总览

| 方法     | 路径                                   | 功能                                |
| -------- | -------------------------------------- | ----------------------------------- |
| `GET`    | `/api/files`                           | 查询视频列表                        |
| `GET`    | `/api/files/{file_id}`                 | 查询视频详情和所有存储路径          |
| `POST`   | `/api/files/{file_id}/identify`        | 模拟识别, 不调用真实 DandanPlay API |
| `GET`    | `/api/files/{file_id}/progress`        | 查询观看进度                        |
| `PUT`    | `/api/files/{file_id}/progress`        | 保存观看进度                        |
| `GET`    | `/api/config`                          | 查询 Web 配置                       |
| `POST`   | `/api/scan`                            | 扫描媒体目录并同步数据库            |
| `GET`    | `/api/files/{file_id}/streams`         | 查询轨道信息, 优先使用探测缓存      |
| `POST`   | `/api/files/{file_id}/probe`           | 强制重新探测轨道信息                |
| `POST`   | `/api/files/{file_id}/playback`        | 创建播放会话并激活共享缓存          |
| `GET`    | `/api/playback/{session_id}/status`    | 查询会话状态并保活                  |
| `POST`   | `/api/playback/{session_id}/heartbeat` | 显式保活                            |
| `POST`   | `/api/playback/{session_id}/target`    | 更新会话目标位置                    |
| `DELETE` | `/api/playback/{session_id}`           | 释放会话租约                        |
| `GET`    | `/playback/{session_id}/{asset}`       | 获取已发布的 HLS 资源               |
| `GET`    | `/health`                              | 检查 HTTP 服务存活                  |

## 文件查询与识别

`GET /api/files` 返回 `200` 和视频记录数组. 每条记录包含 `pre16mib_md5`, `name`, `size`,
`ddp_epi_id`, `danmaku_offset`, `watch_pos`, `watched_datetime`, `path`, `name_no_ext`, `extension`.
大小单位为字节, 观看位置和弹幕偏移单位为秒. 尚无业务数据的可空字段返回 `null`.
同一指纹存在多个副本时, 按路径, 文件名和扩展名排序选择第一条展示, 不重复列出所有副本.

`GET /api/files/{file_id}` 返回 `200`:

```json
{
  "video": {
    "pre16mib_md5": "35af6d912b6dffadc112df2ca94bbe2f",
    "name": "episode.mkv",
    "size": 104857600,
    "ddp_epi_id": null,
    "danmaku_offset": 0.0,
    "watch_pos": 125.5,
    "watched_datetime": "2026-10-10T07:00:00.000Z",
    "path": "series",
    "name_no_ext": "episode",
    "extension": "mkv"
  },
  "paths": [
    {
      "path": "series",
      "name_no_ext": "episode",
      "extension": "mkv",
      "pre16mib_md5": "35af6d912b6dffadc112df2ca94bbe2f"
    }
  ]
}
```

`paths` 包含该指纹的全部数据库路径, `path` 相对于媒体根目录.
详情接口不检查每个副本当前是否仍存在, 也不返回原始媒体播放 URL.
播放前会重新查找可用副本并验证规范化路径仍位于媒体根目录.

`POST /api/files/{file_id}/identify` 无需请求体, 当前只将 `ddp_epi_id` 设置为 `1`.
成功返回 `200` 和 `{"ok":true,"mode":"mock","ddp_epi_id":1}`.
这不是实际番剧识别, 配置 DandanPlay 地址也不会改变此接口的 mock 行为.

## 观看进度

`GET /api/files/{file_id}/progress` 返回 `200`:

```json
{
  "pre16mib_md5": "35af6d912b6dffadc112df2ca94bbe2f",
  "watch_pos": 125.5,
  "watched_datetime": "2026-10-10T07:00:00.000Z"
}
```

未保存进度时, `watch_pos` 和 `watched_datetime` 为 `null`.
`PUT /api/files/{file_id}/progress` 接收 `{"position":125.5}`, 成功返回 `204`, 无响应体.
位置必须为有限且非负的原视频绝对秒数, 当前此接口不校验是否超过视频时长.
保存时同时更新 UTC 观看时间. 进度按文件指纹持久化, 不随会话释放或缓存淘汰删除.

## 配置与扫描

`GET /api/config` 返回 `200` 和 Web 配置, 当前形如 `{"default_dark_mode":true}`.
不会返回播放资源预算, DandanPlay 配置或完整 `config.json`.

`POST /api/scan` 无需请求体, 等待媒体目录收集及数据库同步完成后返回
`200` 和 `{"ok":true,"scanned":744}`. `scanned` 是本次扫描得到的文件路径数量,
同一内容的多个副本分别计数, 不是去重后的指纹数量.
完整收集失败时不会更新数据库路径快照. 同步在事务内替换 `file_path`,
保留 `file` 中的观看进度等业务数据.

## 媒体探测

`GET /api/files/{file_id}/streams` 和 `POST /api/files/{file_id}/probe` 成功均返回
`200` 和媒体信息:

```json
{
  "duration": 1400.0,
  "video_tracks": [
    {
      "stream_index": 0,
      "codec_name": "h264",
      "language": null,
      "title": null,
      "is_default": true,
      "is_forced": false
    }
  ],
  "audio_tracks": [],
  "subtitle_tracks": []
}
```

轨道索引是容器全局流索引, 不是数组下标. 只列出容器内轨道, 不包含外挂字幕,
字体附件和内部源时间偏移. 示例省略了音轨和字幕轨内容.

`streams` 优先复用内存探测结果, 未缓存时执行 ffprobe.
失败结果也会缓存, 后续普通查询返回该错误, 不自动重试.
`probe` 绕过旧结果并重新探测, 无需请求体, 用于显式 Retry probe.
服务重启会清空探测缓存, 两个接口都不启动播放转码.

## 创建播放会话

默认播放只提交文件 ID, 不需要先读取轨道或历史进度:

```bash
curl -X POST http://localhost:56080/api/files/FILE_ID/playback
```

服务端查询文件和 `watch_pos`, 选择第一条视频轨, 默认音轨或第一条音轨,
以及默认或强制字幕. 没有对应轨道时关闭音轨或字幕.
距结尾不足 2 秒的历史进度从 0 开始. 响应同时返回媒体信息及实际选择.

换轨可以使用响应中的真实流索引, 并提交当前绝对播放位置:

```bash
curl -X POST http://localhost:56080/api/files/FILE_ID/playback \
  -H 'Content-Type: application/json' \
  -d '{"audio_stream_index":1,"subtitle_stream_index":3,"position":125.5}'
```

响应:

```json
{
  "session_id": "SESSION_ID",
  "url": "/playback/SESSION_ID/index.m3u8?revision=2",
  "mime_type": "application/vnd.apple.mpegurl",
  "start_time": 0.0,
  "duration": 1400.0,
  "position": 125.5,
  "complete": false,
  "generating": true,
  "stopping": false,
  "ready": false,
  "buffered_until": 0.0,
  "encoding_threads": 8,
  "error": null,
  "media": {
    "duration": 1400.0,
    "video_tracks": [],
    "audio_tracks": [],
    "subtitle_tracks": []
  },
  "selection": {
    "audio": 1,
    "subtitle": 3
  }
}
```

`audio_stream_index: null` 表示无音轨, `subtitle_stream_index: null` 表示关闭字幕.
省略轨道字段则使用服务端默认选择, 和显式 `null` 不同.
客户端不能指定视频索引, 缓存键, 路径或编码配置版本, 未知请求字段会被拒绝.
`position` 必须有限, 非负且小于视频时长; 省略或 `null` 表示读取数据库进度.
位置只影响会话目标和下次启动时的线程申请, 不进入播放配置或缓存键.
示例省略了轨道数组内容. `status` 返回相同的状态字段, 不重复返回 `media` 和 `selection`.

创建成功返回 `200`, 但不表示目标位置已经可播.
同一播放配置的会话共享活动缓存和生成任务, 创建响应会等待磁盘缓存加载完成,
不会等待顺序生成追上目标位置.

## 会话状态, 保活与目标位置

`GET /api/playback/{session_id}/status` 返回 `200` 和上例中的会话状态字段,
不包含 `media` 和 `selection`, 同时刷新存活时间.
`POST /api/playback/{session_id}/heartbeat` 无需请求体, 刷新存活时间后返回 `204`.

等待期间拖动进度条只更新当前会话:

```bash
curl -X POST http://localhost:56080/api/playback/SESSION_ID/target \
  -H 'Content-Type: application/json' -d '{"position":300.5}'
```

`POST /api/playback/{session_id}/target` 接收有限且满足 `0 <= position < duration` 的位置,
拒绝未知字段, 成功返回 `200` 和最新状态. 只改变会话目标, 不保存观看记录,
不创建新会话, 不启动跳转位置的独立任务, 也不热调已运行编码器的线程数.

缓存只有 `complete: true` 和 `false` 两种状态. `generating`, `stopping`,
`encoding_threads` 和 `error` 独立描述任务活动, 不再返回状态枚举.
`generating` 也包括排队, 准备和最终清理, 实际编码器未运行时 `encoding_threads` 为 0.
首次激活先登记唯一活动元素再校验磁盘, 创建响应等待加载结果, 不把完整缓存误报为新空缓存.
尚未返回 ID 的创建预留使用 `max(idle_timeout_seconds, 120)` 秒宽限,
防止等待清理时提前过期, 也避免被中断的创建请求永久占用名额.

前端准备阶段轮询状态,
播放或暂停期间, 生成任务活动时每秒查询状态以刷新缓存进度,
任务结束后每 10 秒查询状态保活; 媒体请求也延长会话有效期.
`buffered_until` 是从时间 0 开始已经生成的范围终点, 不是浏览器本地缓冲长度.
`ready: true` 只表示至少一个分片已通过验证. 目标必须满足 `target < buffered_until`;
相等仍需等待. 已发布前缀可以在后续生成失败后继续播放, 未覆盖目标则明确报错,
需要用户 Retry, 不自动启动新编码器.
最后一个租约释放或过期后, 停止任务并从活动集合移除, 磁盘缓存保留到 LRU 淘汰.

## 释放会话与媒体资源

`DELETE /api/playback/{session_id}` 返回 `204`, 无响应体.
释放操作幂等, 不存在或已经释放的会话也返回成功.
最后一个租约离开会触发后台停止, HTTP 响应不等待编码器完成最后一个分片.
释放租约不会立即删除磁盘缓存, 也不会删除观看记录.

`GET /playback/{session_id}/{asset}` 只允许有效会话读取已发布资源:

| 资源                                | MIME 类型                       |
| ----------------------------------- | ------------------------------- |
| `index.m3u8`                        | `application/vnd.apple.mpegurl` |
| `init.mp4`                          | `video/mp4`                     |
| `segment_000000.m4s` 等六位编号分片 | `video/iso.segment`             |

成功通常返回 `200`, Range 请求可返回 `206`. 底层使用 `ServeFile` 传输资源.
资源接口不接受 variant-key, 不提供源视频直放, 字幕下载或私有工作目录访问.
至少发布一个有效分片后才开放清单和 init, 分片索引必须落在已发布范围内.
响应设置 `Cache-Control: no-store` 和 `X-Content-Type-Options: nosniff`,
避免 HTTP 缓存掩盖 event 清单增长或会话失效.

## 存活检查与静态前端

`GET /health` 在 `backend/src/main.rs` 额外注册, 返回 `200` 和 `{"status":"ok"}`.
只检查 HTTP 服务存活, 不执行媒体探测, 转码或缓存容量统计.
未匹配的路径交给 `/app/web` 静态文件服务, 目录默认尝试 `index.html`,
用于提供前端页面和资源, 不是业务 API.

## 错误响应

业务处理函数生成的错误通常为 `{"error":"具体原因"}`.
Axum 的 JSON 提取错误或静态文件服务错误可能使用框架自身的响应格式.

| 状态码 | 主要情况                                                                   |
| ------ | -------------------------------------------------------------------------- |
| `400`  | 播放请求 JSON 或字段无效, 轨道不存在, 目标位置越界, 保存进度为负或非有限值 |
| `404`  | 文件记录不存在, 源媒体没有可用路径, 会话不存在或过期, 播放资源不可用       |
| `422`  | 播放接口媒体探测失败, 包括复用已缓存的探测错误                             |
| `500`  | 数据库或扫描失败, 会话创建或释放失败, 资源读取等内部错误                   |

错误类型取决于接口中的映射, 不保证每个底层错误都有独立状态码.
异步字幕准备或编码失败由成功状态响应中的 `error` 字段返回,
不表示该 HTTP 状态查询本身失败. 已发布前缀仍可服务, 未覆盖目标需要手动 Retry.
