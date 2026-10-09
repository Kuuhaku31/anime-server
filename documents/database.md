# 数据库结构

`<数据文件夹>/main.db` 是 SQLite 数据库文件, 包含以下表:

## `file`

文件表, 保存文件本身信息, 不关心文件地址.

| 字段             | 类型 | 约束                    | 说明                                                 |
| ---------------- | ---- | ----------------------- | ---------------------------------------------------- |
| pre16mib_md5     | text | primary key             | 文件至多前 16MiB 的 MD5                              |
| size             | int  | not null                | 文件大小, 单位字节                                   |
| ddp_epi_id       | int  |                         | DandanPlay Episode ID, null 表示没有匹配到           |
| danmaku_offset   | real | not null, default = 0.0 | 弹幕偏移, 单位秒, > 0 表示需要弹幕推迟显示, < 0 反之 |
| watch_pos        | real |                         | 播放进度, 单位秒                                     |
| watched_datetime | text |                         | 最近一次播放时间, ISO 8601 格式                      |

## `file_path`

文件路径表, 保存相对于 `/media` 的目录与文件名, 允许同一个文件有多个路径.

| 字段         | 类型 | 约束     | 说明                                        |
| ------------ | ---- | -------- | ------------------------------------------- |
| path         | text | not null | 文件所在目录相对于 media 文件夹的路径       |
| name_no_ext  | text | not null | 文件名, 不包含扩展名                        |
| extension    | text | not null | 文件扩展名, 不包含点                        |
| pre16mib_md5 | text | not null | 文件至多前 16MiB 的 MD5, 外键关联 `file` 表 |

`path`, `name_no_ext`, `extension` 组成复合主键.

该表实时刷新, 保证和 media 文件夹一致.

## 初始化与持久化边界

后端启动时直接执行 `backend/sql/init.sql`, 使用 `CREATE TABLE IF NOT EXISTS`
创建上述两张表及索引. 不使用 SQL 迁移框架.

`watch_pos` 始终保存原视频的绝对播放时间, 不是某个 HLS 会话从零开始的局部时间.
换流, 会话释放和缓存淘汰不会删除进度, 文件识别信息或弹幕偏移.

媒体元数据和探测错误只在服务进程内缓存, 不写入 SQLite.
共享 HLS 缓存在 `/cache/playback/<variant-key>/` 持久保存, 不写入数据库或 JSON 状态文件.
缓存任务状态与租约在内存中维护, 重启时根据完成标记和 HLS 文件重建索引.
字幕和字体中间文件分别位于 `/cache/subtitles` 和 `/cache/fonts`, 生成任务结束后删除.

播放配置使用 `pre16mib_md5` 关联源视频, 轨道选择和编码配置版本用于区分缓存变体.
文件前 16 MiB 之后的改动不会改变此标识, 不会自动使播放缓存失效.
