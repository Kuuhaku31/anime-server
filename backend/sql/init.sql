
CREATE TABLE IF NOT EXISTS file (
    pre16mib_md5 TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    ddp_epi_id INTEGER,
    danmaku_offset REAL NOT NULL DEFAULT 0.0,
    watch_pos REAL,
    watched_datetime TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS file_path (
    path TEXT NOT NULL,
    name_no_ext TEXT NOT NULL,
    extension TEXT NOT NULL,
    pre16mib_md5 TEXT NOT NULL REFERENCES file(pre16mib_md5) ON DELETE CASCADE,
    PRIMARY KEY (path, name_no_ext, extension)
) STRICT;

CREATE INDEX IF NOT EXISTS file_path_pre16mib_md5_idx ON file_path(pre16mib_md5);
