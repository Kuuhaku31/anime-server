//! 封装 SQLite 数据访问操作.

use crate::db::models::{FilePath, ScannedFile, VideoFile, WatchProgress};
use sqlx::SqlitePool;

// 同一指纹对应多个路径时仅展示排序后的第一条, 避免列表重复并保持展示结果稳定.
const VIDEO_SELECT: &str = r#"
    SELECT
        f.pre16mib_md5,
        fp.name_no_ext || '.' || fp.extension AS name,
        f.size,
        f.audio_stream_index,
        f.subtitle_stream_index,
        f.ddp_epi_id,
        f.danmaku_offset,
        f.watch_pos,
        f.watched_datetime,
        fp.path,
        fp.name_no_ext,
        fp.extension
    FROM file AS f
    JOIN file_path AS fp ON fp.rowid = (
        SELECT candidate.rowid
        FROM file_path AS candidate
        WHERE candidate.pre16mib_md5 = f.pre16mib_md5
        ORDER BY candidate.path, candidate.name_no_ext, candidate.extension
        LIMIT 1
    )
"#;

pub async fn list_videos(pool: &SqlitePool) -> Result<Vec<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} ORDER BY fp.name_no_ext, fp.extension, fp.path");
    sqlx::query_as::<_, VideoFile>(&query).fetch_all(pool).await
}

pub async fn get_video(
    pool: &SqlitePool,
    pre16mib_md5: &str,
) -> Result<Option<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} WHERE f.pre16mib_md5 = ?");
    sqlx::query_as::<_, VideoFile>(&query)
        .bind(pre16mib_md5)
        .fetch_optional(pool)
        .await
}

pub async fn get_paths(pool: &SqlitePool, pre16mib_md5: &str) -> Result<Vec<FilePath>, sqlx::Error> {

    sqlx::query_as::<_, FilePath>

("SELECT path, name_no_ext, extension, pre16mib_md5 FROM file_path WHERE pre16mib_md5 = ? ORDER BY path, name_no_ext, extension")

    .bind(pre16mib_md5)
    .fetch_all(pool)
    .await
}

/// 扫描前读取全部路径, 已存在的地址可直接复用而不读取文件内容.
pub async fn list_paths(pool: &SqlitePool) -> Result<Vec<FilePath>, sqlx::Error> {
    sqlx::query_as::<_, FilePath>(
        "SELECT path, name_no_ext, extension, pre16mib_md5 FROM file_path",
    ).fetch_all(pool).await
}

/// 在事务中只插入新路径并删除失效路径, 保留没有现存副本的 file 业务记录.
pub async fn sync_scanned_files(
    pool: &SqlitePool,
    files: &[ScannedFile],
    removed: &[FilePath],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for file in removed {
        sqlx::query("DELETE FROM file_path WHERE path = ? AND name_no_ext = ? AND extension = ?")
            .bind(&file.path)
            .bind(&file.name_no_ext)
            .bind(&file.extension)
            .execute(&mut *tx)
            .await?;
    }

    for file in files {
        sqlx::query(
            "INSERT INTO file(pre16mib_md5, size, audio_stream_index, subtitle_stream_index) VALUES(?, ?, ?, ?) ON CONFLICT(pre16mib_md5) DO UPDATE SET size = excluded.size",
        )
        .bind(&file.pre16mib_md5)
        .bind(file.size)
        .bind(file.audio_stream_index)
        .bind(file.subtitle_stream_index)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO file_path(path, name_no_ext, extension, pre16mib_md5) VALUES(?, ?, ?, ?)",
        )
        .bind(&file.path)
        .bind(&file.name_no_ext)
        .bind(&file.extension)
        .bind(&file.pre16mib_md5)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

/// 只更新请求中显式提供的轨道偏好, 未提供的字段保持数据库原值.
pub async fn save_track_preferences(
    pool: &SqlitePool,
    pre16mib_md5: &str,
    audio: Option<Option<i32>>,
    subtitle: Option<Option<i32>>,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE file SET audio_stream_index = CASE WHEN ? THEN ? ELSE audio_stream_index END, \
         subtitle_stream_index = CASE WHEN ? THEN ? ELSE subtitle_stream_index END WHERE pre16mib_md5 = ?",
    )
    .bind(audio.is_some())
    .bind(audio.flatten())
    .bind(subtitle.is_some())
    .bind(subtitle.flatten())
    .bind(pre16mib_md5)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 { return Err(sqlx::Error::RowNotFound); }
    Ok(())
}

pub async fn set_mock_identification(
    pool: &SqlitePool,
    pre16mib_md5: &str,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE file SET ddp_epi_id = 1 WHERE pre16mib_md5 = ?")
        .bind(pre16mib_md5)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn get_progress(
    pool: &SqlitePool,
    pre16mib_md5: &str,
) -> Result<Option<WatchProgress>, sqlx::Error> {
    sqlx::query_as::<_, WatchProgress>(
        "SELECT pre16mib_md5, watch_pos, watched_datetime, audio_stream_index, subtitle_stream_index FROM file WHERE pre16mib_md5 = ?",
    )
    .bind(pre16mib_md5)
    .fetch_optional(pool)
    .await
}

pub async fn save_progress(
    pool: &SqlitePool,
    pre16mib_md5: &str,
    position: f64,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE file SET watch_pos = ?, watched_datetime = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE pre16mib_md5 = ?",
    )
    .bind(position)
    .bind(pre16mib_md5)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
