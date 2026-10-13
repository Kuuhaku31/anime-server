//! 封装 SQLite 数据访问操作.

use std::{collections::HashMap, path::{Path, PathBuf}};

use crate::models::{ScannedFile, VideoFile, WatchProgress};
use sqlx::SqlitePool;

#[cfg(test)]
#[path = "../tests/db/repository.rs"]
mod tests;

// 列表按地址展示每个副本, 指纹仅用于关联共享的业务数据.
const VIDEO_SELECT: &str = r#"
    SELECT
        f.pre16mib_md5,
        fp.path AS name,
        f.size,
        f.audio_stream_index,
        f.subtitle_stream_index,
        f.ddp_epi_id,
        f.danmaku_offset,
        f.watch_pos,
        f.watched_datetime,
        fp.path
    FROM file AS f
    JOIN file_path AS fp ON fp.pre16mib_md5 = f.pre16mib_md5
"#;


/// 获取全部视频记录, 按完整地址排序, 文件名仅用于显示.
pub(crate) async fn get_video_files(p: &SqlitePool) -> Result<Vec<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} ORDER BY fp.path");
    let mut files = sqlx::query_as::<_, VideoFile>(&query).fetch_all(p).await?;
    for file in &mut files { set_display_name(file)?; }
    Ok(files)
}

/// 获取指定文件指纹的数据库记录.
pub(crate) async fn get_video_by_file_id(
    p: &SqlitePool, file_id: &str,
) -> Result<Option<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} WHERE f.pre16mib_md5 = ? ORDER BY fp.path LIMIT 1");
    let mut file = sqlx::query_as::<_, VideoFile>(&query).bind(file_id).fetch_optional(p).await?;
    if let Some(file) = &mut file { set_display_name(file)?; }
    Ok(file)
}

// 文件名由完整地址生成, 不再保存额外的地址组成字段.
fn set_display_name(file: &mut VideoFile) -> Result<(), sqlx::Error> {
    file.name = Path::new(&file.path).file_name().and_then(|name| name.to_str())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid file path: {}", file.path)))?.to_owned();
    Ok(())
}

pub(crate) async fn get_video_paths_by_file_id(
    p: &SqlitePool, file_id: &str,
) -> Result<Vec<PathBuf>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT path FROM file_path WHERE pre16mib_md5 = ? ORDER BY path")
        .bind(file_id).fetch_all(p).await
        .map(|paths| paths.into_iter().map(PathBuf::from).collect())
}

pub(crate) async fn get_a_video_path_by_file_id(
    p: &SqlitePool, file_id: &str,
) -> Result<Option<PathBuf>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT path FROM file_path WHERE pre16mib_md5 = ? ORDER BY path LIMIT 1")
        .bind(file_id).fetch_optional(p).await
        .map(|opt| opt.map(PathBuf::from))
}

/// 返回文件地址表所有完整文件地址及其对应的文件大小
pub(crate) async fn get_paths_and_size(pool: &SqlitePool) -> Result<HashMap<PathBuf, u64>, sqlx::Error> {
    let records = sqlx::query_as::<_, (String, u64)>(
        "SELECT fp.path, f.size FROM file_path AS fp JOIN file AS f ON f.pre16mib_md5 = fp.pre16mib_md5",
    ).fetch_all(pool).await?;
    Ok(records.into_iter().map(|(path, size)| (PathBuf::from(path), size)).collect())
}

/// 删除指定路径的 file_path 记录
pub(crate) async fn delete_paths(pool: &SqlitePool, paths: &Vec<PathBuf>) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    for path in paths {
        sqlx::query("DELETE FROM file_path WHERE path = ?")
            .bind(path.to_str().ok_or_else(|| sqlx::Error::Protocol(format!("non-UTF-8 file path: {}", path.display())))?)
            .execute(&mut *transaction).await?;
    }
    transaction.commit().await
}

/// 只更新请求中显式提供的轨道偏好, 未提供的字段保持数据库原值.
pub(crate) async fn save_track_preferences(
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

pub(crate) async fn set_mock_identification(
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

pub(crate) async fn get_progress(
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

pub(crate) async fn save_progress(
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

/// 给地址, 删除 file_path 表中指定路径
pub(crate) async fn delete_path(pool: &SqlitePool, path: &Path) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM file_path WHERE path = ?")
    .bind(absolute_file_address(path)?)
    .execute(pool)
    .await?;

    Ok(())
}

// 不读取磁盘, 已消失文件也可以按原绝对地址删除记录.
fn absolute_file_address(path: &Path) -> Result<&str, sqlx::Error> {
    if !path.is_absolute() || path.file_name().is_none()
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(sqlx::Error::Protocol(format!("invalid absolute file path: {}", path.display())));
    }
    path.to_str().ok_or_else(|| sqlx::Error::Protocol(format!("non-UTF-8 file path: {}", path.display())))
}

/// 用 ScannedFile 插入更新 file, file_path 表:
///
/// 1. 插入更新 file 表
/// 2. 插入更新 file_path 表
pub(crate) async fn upsert_file(pool: &SqlitePool, file: &ScannedFile) -> Result<(), sqlx::Error> {

    let path = absolute_file_address(file.path())?;
    let mut transaction = pool.begin().await?;

    // 文件指纹冲突仅更新大小, 不覆盖观看进度, 识别信息或轨道偏好.
    sqlx::query(
        "INSERT INTO file (pre16mib_md5, size) VALUES (?, ?) \
         ON CONFLICT (pre16mib_md5) DO UPDATE SET size = excluded.size",
    )
        .bind(file.pre16mib_md5())
        .bind(file.size())
        .execute(&mut *transaction)
        .await?;

    // 地址冲突直接更新指纹关联, 保留该路径行以及旧指纹的业务记录.
    sqlx::query(
        "INSERT INTO file_path (path, pre16mib_md5) VALUES (?, ?) \
         ON CONFLICT (path) DO UPDATE SET pre16mib_md5 = excluded.pre16mib_md5",
    )
        .bind(path)
        .bind(file.pre16mib_md5())
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await
}
