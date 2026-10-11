//! 封装 SQLite 数据访问操作.

use std::path::PathBuf;

use crate::{models::{ScanChanges, VideoFile, WatchProgress}};
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


/// 获取全部视频记录, 按路径, 文件名和扩展名排序.
pub async fn get_videos(p: &SqlitePool) -> Result<Vec<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} ORDER BY fp.name_no_ext, fp.extension, fp.path");
    sqlx::query_as::<_, VideoFile>(&query).fetch_all(p).await
}

/// 获取指定文件指纹的数据库记录.
pub async fn get_video_by_id(p: &SqlitePool, id: &str) -> Result<Option<VideoFile>, sqlx::Error> {
    let query = format!("{VIDEO_SELECT} WHERE f.pre16mib_md5 = ?");
    sqlx::query_as::<_, VideoFile>(&query)
    .bind(id).fetch_optional(p).await
}

pub async fn get_video_paths_by_id(p: &SqlitePool, id: &str) -> Result<Vec<PathBuf>, sqlx::Error> {

    sqlx::query_as::<_, (String, String, String)>

("SELECT path, name_no_ext, extension FROM file_path WHERE pre16mib_md5 = ? ORDER BY path, name_no_ext, extension")

    .bind(id)
    .fetch_all(p).await
    .map(|file_paths| file_paths.into_iter().map(|(path, name_no_ext, extension)| {
        let mut path_buf = PathBuf::from(path);
        path_buf.push(format!("{}.{}", name_no_ext, extension));
        path_buf
    }).collect::<Vec<PathBuf>>())
}

/// 返回文件地址表所有完整文件地址, 包含目录, 文件名和扩展名.
pub async fn get_paths(pool: &SqlitePool) -> Result<Vec<PathBuf>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, String, String)>
    ("SELECT path, name_no_ext, extension, pre16mib_md5 FROM file_path")
    .fetch_all(pool).await
    .map(|file_paths| file_paths.into_iter().map(|(path, name_no_ext, extension, _)| {
        let mut path_buf = PathBuf::from(path);
        path_buf.push(format!("{}.{}", name_no_ext, extension));
        path_buf
    }).collect::<Vec<PathBuf>>())
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


/// 根据 ScanChanges 更新 file, file_path 表:
///
/// 1. 删除 file_path 表中 ScanChanges.removed 中的路径
/// 2. 把 ScanChanges.added 中的文件插入更新 file 表
/// 3. 把 ScanChanges.added 中的文件路径插入 file_path 表
pub(crate) async fn update(pool: &SqlitePool, changes: &ScanChanges) -> Result<(), sqlx::Error> {

    let mut transaction = pool.begin().await?;

    // 按完整复合键删除失效路径, 不删除 file 记录和同目录其他文件.
    for path in &changes.removed {
        let directory = path.parent().and_then(|d| d.to_str())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", path.display()).into()))?;
        let name_no_ext = path.file_stem().and_then(|s| s.to_str())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", path.display()).into()))?;
        let extension = path.extension().and_then(|s| s.to_str())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", path.display()).into()))?;

        sqlx::query("DELETE FROM file_path WHERE path = ? AND name_no_ext = ? AND extension = ?")
        .bind(directory)
        .bind(name_no_ext)
        .bind(extension)
        .execute(&mut *transaction)
        .await?;
    }

    // 独立写入文件表, 已有指纹只更新大小, 保留轨道偏好和全部业务数据.
    for file in &changes.added {
        sqlx::query(
            "INSERT INTO file (pre16mib_md5, size) VALUES (?, ?) \
             ON CONFLICT (pre16mib_md5) DO UPDATE SET size = excluded.size",
        )
            .bind(file.pre16mib_md5())
            .bind(file.size())
            .execute(&mut *transaction)
            .await?;
    }

    // 写入新增路径, 主键冲突或无效路径使整个事务回滚.
    for file in &changes.added {
        let directory = file.file_dir().and_then(|d| d.to_str())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", file.path().display()).into()))?;
        let name_no_ext = file.file_name_no_ext().ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", file.path().display()).into()))?;
        let extension = file.file_extension().ok_or_else(|| sqlx::Error::Protocol(format!("invalid path: {}", file.path().display()).into()))?;

        sqlx::query("INSERT INTO file_path (path, name_no_ext, extension, pre16mib_md5) VALUES (?, ?, ?, ?)")
        .bind(directory)
        .bind(name_no_ext)
        .bind(extension)
        .bind(file.pre16mib_md5())
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScannedFile;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn changes_update_exact_paths_and_preserve_file_preferences() {
        // 删除一个已消失副本, 其同目录兄弟路径及共享文件业务数据必须保留.
        let root = std::env::temp_dir().join(format!("anime-update-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let result = async {
            let first = root.join("episode.part.MKV");
            let second = root.join("copy.mkv");
            std::fs::write(&first, b"same media").unwrap();
            std::fs::write(&second, b"same media").unwrap();
            let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
            sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
            let changes = ScanChanges {
                added: vec![ScannedFile::new(&first).unwrap(), ScannedFile::new(&second).unwrap()],
                removed: Vec::new(), count: 2,
            };
            let id = changes.added[0].pre16mib_md5().to_owned();
            update(&pool, &changes).await.unwrap();
            assert_eq!(get_paths(&pool).await.unwrap().len(), 2);
            assert!(get_paths(&pool).await.unwrap().contains(&first));
            // 空变更不应删除并重建已有路径行.
            let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM file_path WHERE name_no_ext = 'episode.part'")
                .fetch_one(&pool).await.unwrap();
            update(&pool, &ScanChanges {
                added: Vec::new(), removed: Vec::new(), count: 2,
            }).await.unwrap();
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT rowid FROM file_path WHERE name_no_ext = 'episode.part'")
                .fetch_one(&pool).await.unwrap(), rowid);
            save_progress(&pool, &id, 12.5).await.unwrap();
            save_track_preferences(&pool, &id, Some(Some(1)), Some(Some(3))).await.unwrap();
            set_mock_identification(&pool, &id).await.unwrap();
            let moved = root.join("renamed.mp4");
            std::fs::rename(&first, &moved).unwrap();
            let changes = ScanChanges {
                added: vec![ScannedFile::new(&moved).unwrap()], removed: vec![first], count: 2,
            };
            update(&pool, &changes).await.unwrap();
            assert!(get_paths(&pool).await.unwrap().contains(&second));
            assert!(get_paths(&pool).await.unwrap().contains(&moved));
            let progress = get_progress(&pool, &id).await.unwrap().unwrap();
            assert_eq!(progress.watch_pos, Some(12.5));
            assert_eq!(progress.audio_stream_index, Some(1));
            assert_eq!(progress.subtitle_stream_index, Some(3));
            assert_eq!(get_video_by_id(&pool, &id).await.unwrap().unwrap().ddp_epi_id, Some(1));
            update(&pool, &ScanChanges {
                added: Vec::new(), removed: vec![second.clone(), moved], count: 0,
            }).await.unwrap();
            assert!(get_paths(&pool).await.unwrap().is_empty());
            assert!(get_progress(&pool, &id).await.unwrap().is_some());
            // 全部路径删除后重新登记同内容文件, 仍恢复原业务记录.
            update(&pool, &ScanChanges {
                added: vec![ScannedFile::new(&second).unwrap()], removed: Vec::new(), count: 1,
            }).await.unwrap();
            let restored = get_video_by_id(&pool, &id).await.unwrap().unwrap();
            assert_eq!(restored.watch_pos, Some(12.5));
            assert_eq!(restored.audio_stream_index, Some(1));
            assert_eq!(restored.subtitle_stream_index, Some(3));
            assert_eq!(restored.ddp_epi_id, Some(1));
        }.await;
        std::fs::remove_dir_all(&root).unwrap();
        result
    }

    #[tokio::test]
    async fn changes_roll_back_deletions_if_an_insert_fails() {
        // 重复新增地址触发复合主键冲突, 前面的路径删除和新文件插入必须一起回滚.
        let root = std::env::temp_dir().join(format!("anime-update-rollback-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let old = root.join("old.mkv");
        let new = root.join("new.mkv");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
        update(&pool, &ScanChanges {
            added: vec![ScannedFile::new(&old).unwrap()], removed: Vec::new(), count: 1,
        }).await.unwrap();
        let added = vec![ScannedFile::new(&new).unwrap(), ScannedFile::new(&new).unwrap()];
        let id = added[0].pre16mib_md5().to_owned();
        assert!(update(&pool, &ScanChanges { added, removed: vec![old.clone()], count: 1 }).await.is_err());
        assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
        assert!(get_progress(&pool, &id).await.unwrap().is_none());
        // 在有效删除后遇到无效地址, 删除操作也不能残留.
        for invalid in ["/", "/media/video"] {
            assert!(update(&pool, &ScanChanges {
                added: Vec::new(), removed: vec![old.clone(), PathBuf::from(invalid)], count: 0,
            }).await.is_err());
            assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
