# 持久化播放缓存计划

## 目标

将当前按客户端会话创建并删除的临时 HLS, 改为挂载在 `/cache` 的共享持久缓存.

- 每个播放配置生成一份完整的 H.264/AAC fMP4 HLS.
- 从媒体时间 0 开始连续生成, 每个 `.m4s` 分片约 4 秒.
- 首次请求在目标位置已有足够分片后开始播放.
- 最后一个客户端离开后, FFmpeg 仍继续生成到完成.
- 相同播放配置的并发请求共享一个 FFmpeg 任务和一份缓存.
- 只支持视频容器内的字幕轨和字体附件.
- 浏览器解码失败时直接显示错误, 不自动请求其他格式.

## 播放配置与缓存键

服务端校验文件 ID 和轨道选择后, 由 Rust 构造"播放配置":

```rust
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct PlaybackVariantConfig {
    src_file_pre16mib_md5: String,
    video_stream: i32,
    audio_stream: Option<i32>,
    subtitle_stream: Option<i32>,
    encoder_profile: String,
}
```

`audio_stream` 和 `subtitle_stream` 可以为 `null`.
客户端不能指定 `encoder_profile`, `variant-key` 或缓存路径.

Rust 使用 `serde_json::to_vec` 按结构体字段顺序序列化播放配置,
再计算 JSON 字节的 MD5, 得到 32 位小写 `variant-key`.

```text
播放配置 --MD5--> variant-key --目录名--> 一份视频缓存
```

同一播放配置始终得到同一键. 任一字段变化都会对应另一份缓存.
文件移动或改名不改变前 16 MiB MD5, 可以继续复用缓存.
如果只修改文件前 16 MiB 之后的内容, 缓存不会自动失效,
需要删除对应缓存或更新 `encoder_profile`.

## 目录结构

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

- `playback/<variant-key>/` 只保存 HLS 和空的 `complete` 标记.
- 不保存 JSON 状态文件, 运行状态只保存在内存中.
- FFmpeg 成功结束且最终 HLS 验证通过后, 原子创建 `complete`.
- 重启时删除没有 `complete` 或 HLS 校验失败的变体目录.
- 已生成时长从 `index.m3u8` 计算, 大小通过扫描缓存文件计算.

## 字幕与字体

- 文本字幕只从容器内选定字幕轨提取到 `/cache/subtitles/<random-id>.ass`.
- 字体只从容器附件提取到 `/cache/fonts/<generation-id>/`.
- 位图字幕直接参与 FFmpeg 渲染, 不创建 ASS.
- 每个生成任务使用独立临时名称和字体目录.
- 客户端释放会话不删除生成任务仍在使用的中间文件.
- FFmpeg 完成, 失败或取消后立即删除对应字幕和字体中间文件.
- 启动时清理异常退出遗留的中间文件.

## 生命周期与淘汰

- 会话只是缓存租约, 释放会话不删除缓存, 也不停止后台生成.
- 同一 `variant-key` 在内存中只允许一个生成任务.
- 完整缓存跨服务重启和容器重建复用.
- 通过目录修改时间记录跨重启的最后使用时间, 高频分片请求采用防抖更新.
- LRU 只淘汰没有活动租约的完整缓存.
- 正在生成或正在使用的缓存不淘汰, 允许容量暂时超过上限.
- 条目完成或变为非活动后执行清理, 直到总占用不超过配置值.
- 字幕和字体临时占用计入总容量, 但在用中间文件不参与 LRU.

## 配置与挂载

`docker-compose.yml` 新增:

```yaml
volumes:
  - "${CACHE_DIR}:/cache"
```

`examples/.env.example` 新增:

```dotenv
CACHE_DIR=/absolute/path/to/cache
```

`playback` 配置新增:

```json
{
  "cache_max_bytes": 10737418240
}
```

默认上限是 10 GiB. `/data` 继续保存 SQLite 和 API 数据.

## 实施步骤

1. 在 `backend/src/config.rs` 增加缓存目录和容量配置.
2. 新增 `backend/src/video/cache.rs`, 实现:
   - 播放配置和 `variant-key`.
   - 共享生成任务和会话租约.
   - 启动扫描和 HLS 完整性校验.
   - 容量统计, 目录使用时间和 LRU.
   - `complete` 原子创建.
3. 重构 `sessions.rs`, 使客户端会话引用共享缓存条目.
4. 重构 `ffmpeg.rs`, 始终从时间 0 生成完整缓存, 首段验证后允许播放.
5. 重构 `metadata.rs` 和 `assets.rs`, 只处理容器内字幕和字体附件, 并管理临时文件.
6. 调整播放 API:
   - `playback-plan` 返回缓存租约.
   - `buffered_until` 报告共享缓存生成位置.
   - 删除会话只释放租约.
   - 只提供允许列表中的 HLS 文件.
7. 调整前端:
   - `start_time` 固定为 0.
   - 等待 `buffered_until` 覆盖目标位置后加载 HLS.
   - 跳转到未生成范围时等待当前缓存, 不创建按起点区分的缓存.
   - 切换音轨或字幕时切换播放配置, 保留位置和播放状态.
8. 更新 Compose, 示例配置, README 和 `documents/`.

## 验证

- 相同播放配置只启动一个 FFmpeg, 不同轨道配置生成不同缓存.
- 输出由 `init.mp4` 和连续 `.m4s` 分片组成.
- 首段可播放时后台继续生成, 客户端离开后仍生成到完成.
- 容器内文本字幕, 位图字幕, 字体附件和关闭字幕均正确.
- 中间文件在任务结束后删除, 完整 HLS 保留.
- 重启后复用完整缓存, 删除未完成缓存和遗留中间文件.
- LRU 不删除活动或生成中的缓存, 条目空闲后将容量降到上限以内.
- 恢复播放和换轨保持准确位置及播放状态.
- 浏览器错误不触发格式回退.

## 约束

- 不引入 SQL migration.
- 不使用 `mod.rs`.
- 不运行格式化工具.
- 不更新依赖版本.
- 不支持容器外字幕.
