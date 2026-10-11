
-- backend/sql/init.sql
-- 创建数据库表和索引, 用于存储文件信息和路径信息.


-- 文件表
CREATE TABLE IF NOT EXISTS "file"
(
    "pre16mib_md5"          TEXT PRIMARY KEY,
    "size"                  INTEGER NOT NULL,
    "audio_stream_index"    INTEGER,
    "subtitle_stream_index" INTEGER,
    "ddp_epi_id"            INTEGER,
    "danmaku_offset"        REAL NOT NULL DEFAULT 0.0,
    "watch_pos"             REAL,
    "watched_datetime"      TEXT
)
STRICT;

-- 文件路径表
CREATE TABLE IF NOT EXISTS "file_path"
(
    "path"          TEXT NOT NULL,
    "name_no_ext"   TEXT NOT NULL,
    "extension"     TEXT NOT NULL,
    "pre16mib_md5"  TEXT NOT NULL
    REFERENCES "file"("pre16mib_md5") ON DELETE CASCADE,

    PRIMARY KEY ("path", "name_no_ext", "extension")
)
STRICT;

-- 索引, 用于加速查询文件路径表中的 pre16mib_md5 字段.
CREATE INDEX IF NOT EXISTS
"file_path_pre16mib_md5_idx" ON "file_path"("pre16mib_md5");
