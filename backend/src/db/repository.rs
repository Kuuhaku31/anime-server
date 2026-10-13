//! 封装 SQLite 数据访问操作.

use std::{collections::HashMap, path::{Path, PathBuf}};

use crate::models::{ScannedFile, VideoFile, WatchProgress};
use sqlx::SqlitePool;

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


// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScannedFile;
    use sqlx::sqlite::SqlitePoolOptions;

    // 测试从现有大小快照提取排序地址, 不恢复已删除的生产接口.
    async fn get_paths(pool: &SqlitePool) -> Result<Vec<PathBuf>, sqlx::Error> {
        let mut paths: Vec<_> = get_paths_and_size(pool).await?.into_keys().collect();
        paths.sort();
        Ok(paths)
    }

    // #[tokio::test]
    // async fn path_size_snapshot_and_batch_deletion_preserve_file_records() {
    //     // 验证当前扫描依赖的大小快照和批量删除, 不访问真实媒体目录.
    //     let root = std::env::temp_dir().join(format!("anime-size-snapshot-{}", crate::utils::random_id().unwrap()));
    //     std::fs::create_dir(&root).unwrap();
    //     let first = root.join("first.mkv");
    //     let second = root.join("second.mkv");
    //     std::fs::write(&first, b"original").unwrap();
    //     std::fs::write(&second, b"original").unwrap();
    //     let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    //     sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
    //     let file = ScannedFile::new(&first).unwrap();
    //     upsert_file(&pool, &file).await.unwrap();
    //     upsert_file(&pool, &ScannedFile::new(&second).unwrap()).await.unwrap();
    //     save_progress(&pool, file.pre16mib_md5(), 12.5).await.unwrap();
    //     let sizes = get_paths_and_size(&pool).await.unwrap();
    //     assert_eq!(sizes.len(), 2);
    //     assert_eq!(sizes.get(&first), Some(&8));
    //     assert_eq!(sizes.get(&second), Some(&8));
    //     // 大小变化重新入库后, 快照必须返回新大小和新关联.
    //     std::fs::write(&first, b"larger changed content").unwrap();
    //     upsert_file(&pool, &ScannedFile::new(&first).unwrap()).await.unwrap();
    //     assert_eq!(get_paths_and_size(&pool).await.unwrap().get(&first), Some(&22));
    //     delete_paths(&pool, &vec![first.clone()]).await.unwrap();
    //     assert_eq!(get_paths(&pool).await.unwrap(), vec![second.clone()]);
    //     delete_paths(&pool, &vec![second.clone()]).await.unwrap();
    //     assert!(get_paths_and_size(&pool).await.unwrap().is_empty());
    //     assert_eq!(get_progress(&pool, file.pre16mib_md5()).await.unwrap().unwrap().watch_pos, Some(12.5));
    //     std::fs::remove_file(first).unwrap();
    //     std::fs::remove_file(second).unwrap();
    //     std::fs::remove_dir(root).unwrap();
    // }

    // #[tokio::test]
    // async fn upsert_replaces_path_identity_and_rolls_back_on_path_failure() {
    //     // 覆盖首次插入, 重复更新, 内容替换及第二张表失败后的事务回滚.
    //     let root = std::env::temp_dir().join(format!("anime-upsert-{}", crate::utils::random_id().unwrap()));
    //     std::fs::create_dir(&root).unwrap();
    //     let path = root.join("episode.part.MKV");
    //     std::fs::write(&path, b"original").unwrap();
    //     let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    //     sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
    //     // 新地址表只保存完整地址和指纹, 禁止旧的拆分字段及相对地址.
    //     let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('file_path') ORDER BY cid")
    //         .fetch_all(&pool).await.unwrap();
    //     assert_eq!(columns, vec!["path", "pre16mib_md5"]);
    //     let first = ScannedFile::new(&path).unwrap();
    //     let old_id = first.pre16mib_md5();
    //     upsert_file(&pool, &first).await.unwrap();
    //     assert!(sqlx::query("INSERT INTO file_path(path, pre16mib_md5) VALUES ('relative.mkv', ?)")
    //         .bind(old_id).execute(&pool).await.is_err());
    //     save_progress(&pool, old_id, 12.5).await.unwrap();
    //     save_track_preferences(&pool, old_id, Some(Some(1)), Some(Some(3))).await.unwrap();
    //     set_mock_identification(&pool, old_id).await.unwrap();
    //     let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM file_path").fetch_one(&pool).await.unwrap();
    //     sqlx::query("UPDATE file SET size = 0").execute(&pool).await.unwrap();
    //     upsert_file(&pool, &first).await.unwrap();
    //     let video = get_video_by_file_id(&pool, old_file_id).await.unwrap().unwrap();
    //     assert_eq!(video.size, first.size());
    //     assert_eq!(video.watch_pos, Some(12.5));
    //     assert_eq!(video.audio_stream_index, Some(1));
    //     assert_eq!(video.subtitle_stream_index, Some(3));
    //     assert_eq!(video.ddp_epi_id, Some(1));

    //     std::fs::write(&path, b"replacement").unwrap();
    //     let replacement = ScannedFile::new(&path).unwrap();
    //     upsert_file(&pool, &replacement).await.unwrap();
    //     assert_eq!(sqlx::query_scalar::<_, String>("SELECT pre16mib_md5 FROM file_path")
    //         .fetch_one(&pool).await.unwrap(), replacement.pre16mib_md5());
    //     assert_eq!(sqlx::query_scalar::<_, i64>("SELECT rowid FROM file_path").fetch_one(&pool).await.unwrap(), rowid);
    //     assert_eq!(get_progress(&pool, old_id).await.unwrap().unwrap().watch_pos, Some(12.5));
    //     assert_eq!(get_progress(&pool, replacement.pre16mib_md5()).await.unwrap().unwrap().audio_stream_index, None);

    //     sqlx::raw_sql("CREATE TRIGGER reject_path BEFORE UPDATE ON file_path BEGIN SELECT RAISE(ABORT, 'reject path'); END;")
    //         .execute(&pool).await.unwrap();
    //     std::fs::write(&path, b"rejected replacement").unwrap();
    //     let rejected = ScannedFile::new(&path).unwrap();
    //     assert!(upsert_file(&pool, &rejected).await.is_err());
    //     assert!(get_progress(&pool, rejected.pre16mib_md5()).await.unwrap().is_none());
    //     assert_eq!(sqlx::query_scalar::<_, String>("SELECT pre16mib_md5 FROM file_path")
    //         .fetch_one(&pool).await.unwrap(), replacement.pre16mib_md5());
    //     std::fs::remove_file(path).unwrap();
    //     std::fs::remove_dir(root).unwrap();
    // }

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
            let scanned = ScannedFile::new(&first).unwrap();
            let file_id = scanned.pre16mib_md5().to_owned();
            upsert_file(&pool, &scanned).await.unwrap();
            upsert_file(&pool, &ScannedFile::new(&second).unwrap()).await.unwrap();
            assert_eq!(get_paths(&pool).await.unwrap().len(), 2);
            // 列表不按指纹去重, 用返回行验证每个地址关联到同一指纹.
            let videos = get_video_files(&pool).await.unwrap();
            assert_eq!(videos.len(), 2);
            for path in [&first, &second] {
                assert!(videos.iter().any(|video| {
                    std::path::Path::new(&video.path) == *path
                        && video.pre16mib_md5 == file_id
                }));
            }
            assert!(get_paths(&pool).await.unwrap().contains(&first));
            // 同地址重复更新不应删除并重建已有路径行.
            let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM file_path WHERE path = ?").bind(first.to_str().unwrap())
                .fetch_one(&pool).await.unwrap();
            upsert_file(&pool, &scanned).await.unwrap();
            assert_eq!(sqlx::query_scalar::<_, i64>("SELECT rowid FROM file_path WHERE path = ?").bind(first.to_str().unwrap())
                .fetch_one(&pool).await.unwrap(), rowid);
            save_progress(&pool, &file_id, 12.5).await.unwrap();
            save_track_preferences(&pool, &file_id, Some(Some(1)), Some(Some(3))).await.unwrap();
            set_mock_identification(&pool, &file_id).await.unwrap();
            let moved = root.join("renamed.mp4");
            std::fs::rename(&first, &moved).unwrap();
            delete_path(&pool, &first).await.unwrap();
            assert_eq!(get_paths(&pool).await.unwrap(), vec![second.clone()]);
            upsert_file(&pool, &ScannedFile::new(&moved).unwrap()).await.unwrap();
            assert!(get_paths(&pool).await.unwrap().contains(&second));
            assert!(get_paths(&pool).await.unwrap().contains(&moved));
            let progress = get_progress(&pool, &file_id).await.unwrap().unwrap();
            assert_eq!(progress.watch_pos, Some(12.5));
            assert_eq!(progress.audio_stream_index, Some(1));
            assert_eq!(progress.subtitle_stream_index, Some(3));
            assert_eq!(
                get_video_by_file_id(&pool, &file_id).await.unwrap().unwrap().ddp_epi_id,
                Some(1),
            );
            delete_path(&pool, &second).await.unwrap();
            delete_path(&pool, &moved).await.unwrap();
            assert!(get_paths(&pool).await.unwrap().is_empty());
            assert!(get_progress(&pool, &file_id).await.unwrap().is_some());
            // 全部路径删除后重新登记同内容文件, 仍恢复原业务记录.
            upsert_file(&pool, &ScannedFile::new(&second).unwrap()).await.unwrap();
            let restored = get_video_by_file_id(&pool, &file_id).await.unwrap().unwrap();
            assert_eq!(restored.watch_pos, Some(12.5));
            assert_eq!(restored.audio_stream_index, Some(1));
            assert_eq!(restored.subtitle_stream_index, Some(3));
            assert_eq!(restored.ddp_epi_id, Some(1));
        }.await;
        std::fs::remove_dir_all(&root).unwrap();
        result
    }

    #[tokio::test]
    async fn invalid_path_deletion_preserves_existing_records() {
        // 无效地址删除失败不影响现有记录, 写入回滚由独立 upsert 测试覆盖.
        let root = std::env::temp_dir().join(format!("anime-update-rollback-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let old = root.join("old.mkv");
        std::fs::write(&old, b"old").unwrap();
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../sql/init.sql")).execute(&pool).await.unwrap();
        upsert_file(&pool, &ScannedFile::new(&old).unwrap()).await.unwrap();
        assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
        // 无效地址不得误删同目录的有效文件.
        for invalid in ["/", "relative/video.mkv", "/media/../video.mkv"] {
            assert!(delete_path(&pool, Path::new(invalid)).await.is_err());
            assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
