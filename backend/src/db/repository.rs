
// backend/src/db/repository.rs
//! 封装媒体路径关联和共享播放配置的事务操作.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::models::{FileRecord, MediaConfigs, PathConfigs, PathRecord, ScannedFile};

/// 聚合路径和内容配置, 不在数据库中保存媒体探测快照.
pub(super) async fn list_paths_with_configs(pool: &SqlitePool) -> Result<Vec<PathConfigs>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, Option<f64>, Option<i32>, Option<i32>, Option<i32>)>(
        "SELECT fp.path, f.position, f.video_stream_index, f.audio_stream_index, f.subtitle_stream_index \
         FROM file_path AS fp JOIN file AS f ON f.media_id = fp.media_id ORDER BY fp.path",
    ).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(path, position, video_stream_index, audio_stream_index, subtitle_stream_index)| PathConfigs {
        path: PathBuf::from(path),
        configs: MediaConfigs { position, video_stream_index, audio_stream_index, subtitle_stream_index },
    }).collect())
}

/// 返回扫描器使用的路径和内容标识.
pub(super) async fn list_paths(pool: &SqlitePool) -> Result<Vec<PathRecord>, sqlx::Error> {
    sqlx::query_as("SELECT media_id, path FROM file_path ORDER BY path").fetch_all(pool).await
}

/// 查询路径当前关联的内容配置.
pub(super) async fn get_file_by_path(pool: &SqlitePool, path: &Path) -> Result<Option<FileRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT f.media_id, f.position, f.video_stream_index, f.audio_stream_index, \
         f.subtitle_stream_index FROM file AS f JOIN file_path AS fp \
         ON fp.media_id = f.media_id WHERE fp.path = ?",
    ).bind(absolute_file_address(path)?).fetch_optional(pool).await
}

/// 按路径替换配置, 未登记路径明确返回 RowNotFound.
pub(super) async fn save_configs_by_path(pool: &SqlitePool, path: &Path, configs: &MediaConfigs) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE file SET position = ?, video_stream_index = ?, audio_stream_index = ?, \
         subtitle_stream_index = ? WHERE media_id = (SELECT media_id FROM file_path WHERE path = ?)",
    ).bind(configs.position).bind(configs.video_stream_index).bind(configs.audio_stream_index)
        .bind(configs.subtitle_stream_index).bind(absolute_file_address(path)?).execute(pool).await?;
    if result.rows_affected() == 0 { return Err(sqlx::Error::RowNotFound); }
    Ok(())
}

/// 原子应用扫描关联.
pub(super) async fn replace_scanned_paths(
    pool: &SqlitePool, files: &[ScannedFile], removed: &[PathBuf],
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    for path in removed {
        sqlx::query("DELETE FROM file_path WHERE path = ?")
            .bind(absolute_file_address(path)?).execute(&mut *transaction).await?;
    }
    for file in files {
        let path = absolute_file_address(&file.path)?;
        sqlx::query("DELETE FROM file_path WHERE path = ?").bind(path).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO file (media_id) VALUES (?) ON CONFLICT (media_id) DO NOTHING")
            .bind(&file.media_id).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO file_path (media_id, path) VALUES (?, ?)")
            .bind(&file.media_id).bind(path).execute(&mut *transaction).await?;
    }
    transaction.commit().await
}

/// 只校验地址形状, 允许按原地址删除已经消失的文件记录.
fn absolute_file_address(path: &Path) -> Result<&str, sqlx::Error> {
    if !path.is_absolute() || path.file_name().is_none()
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(sqlx::Error::Protocol(format!("invalid absolute media path: {}", path.display())));
    }
    path.to_str().ok_or_else(|| sqlx::Error::Protocol(format!("non-UTF-8 media path: {}", path.display())))
}
