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

文件路径表, 保存文件的绝对路径, 允许同一个文件有多个路径.

| 字段         | 类型 | 约束                  | 说明                                        |
| ------------ | ---- | --------------------- | ------------------------------------------- |
| path         | text | not null              | 文件所在目录相对于 media 文件夹的路径       |
| name_no_ext  | text | not null              | 文件名, 不包含扩展名                        |
| extension    | text | not null              | 文件扩展名, 不包含点                        |
| pre16mib_md5 | text | not null              | 文件至多前 16MiB 的 MD5, 外键关联 `file` 表 |

`path`, `name_no_ext`, `extension` 组成复合主键.

该表实时刷新, 保证和 media 文件夹一致.
