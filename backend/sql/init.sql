
-- backend/sql/init.sql
-- 创建媒体文件配置, 剧集关联和媒体路径表.

CREATE TABLE IF NOT EXISTS "file"
(
    "media_id"              TEXT PRIMARY KEY,
    "position"              REAL CHECK ("position" >= 0),
    "video_stream_index"    INTEGER CHECK ("video_stream_index" >= 0),
    "audio_stream_index"    INTEGER CHECK ("audio_stream_index" >= 0),
    "subtitle_stream_index" INTEGER CHECK ("subtitle_stream_index" >= 0),
    -- 同内容副本共享的 DandanPlay 剧集 ID, NULL 表示尚未关联.
    "episode_id"            INTEGER CHECK ("episode_id" > 0)
)
STRICT;

CREATE TABLE IF NOT EXISTS "file_path"
(
    "media_id"  TEXT NOT NULL REFERENCES "file"("media_id") ON DELETE CASCADE,
    "path"      TEXT NOT NULL CHECK (substr("path", 1, 1) = '/'),
    PRIMARY KEY ("media_id", "path")
)
STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS "file_path_path_idx" ON "file_path"("path");
CREATE INDEX IF NOT EXISTS "file_path_media_id_idx" ON "file_path"("media_id");
