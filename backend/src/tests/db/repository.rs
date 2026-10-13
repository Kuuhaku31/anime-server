//! 校验 SQLite 仓储的路径同步, 事务和业务数据保留.

use super::*;
use crate::models::ScannedFile;
use sqlx::sqlite::SqlitePoolOptions;

// 测试从现有大小快照提取排序地址, 不恢复已删除的生产接口.
async fn get_paths(pool: &SqlitePool) -> Result<Vec<PathBuf>, sqlx::Error> {
    let mut paths: Vec<_> = get_paths_and_size(pool).await?.into_keys().collect();
    paths.sort();
    Ok(paths)
}

// 大小快照随重新入库更新, 批量删除路径不删除文件业务记录.
#[tokio::test]
async fn path_size_snapshot_and_batch_deletion_preserve_file_records() {
    // 验证当前扫描依赖的大小快照和批量删除, 不访问真实媒体目录.
    let root = std::env::temp_dir().join(format!("anime-size-snapshot-{}", crate::utils::random_id().unwrap()));
    std::fs::create_dir(&root).unwrap();
    let first = root.join("first.mkv");
    let second = root.join("second.mkv");
    std::fs::write(&first, b"original").unwrap();
    std::fs::write(&second, b"original").unwrap();
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../../../sql/init.sql")).execute(&pool).await.unwrap();
    let file = ScannedFile::new(&first).unwrap();
    upsert_file(&pool, &file).await.unwrap();
    upsert_file(&pool, &ScannedFile::new(&second).unwrap()).await.unwrap();
    save_progress(&pool, file.pre16mib_md5(), 12.5).await.unwrap();
    let sizes = get_paths_and_size(&pool).await.unwrap();
    assert_eq!(sizes.len(), 2);
    assert_eq!(sizes.get(&first), Some(&8));
    assert_eq!(sizes.get(&second), Some(&8));
    // 大小变化重新入库后, 快照必须返回新大小和新关联.
    std::fs::write(&first, b"larger changed content").unwrap();
    upsert_file(&pool, &ScannedFile::new(&first).unwrap()).await.unwrap();
    assert_eq!(get_paths_and_size(&pool).await.unwrap().get(&first), Some(&22));
    delete_paths(&pool, &vec![first.clone()]).await.unwrap();
    assert_eq!(get_paths(&pool).await.unwrap(), vec![second.clone()]);
    delete_paths(&pool, &vec![second.clone()]).await.unwrap();
    assert!(get_paths_and_size(&pool).await.unwrap().is_empty());
    assert_eq!(get_progress(&pool, file.pre16mib_md5()).await.unwrap().unwrap().watch_pos, Some(12.5));
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();
    std::fs::remove_dir(root).unwrap();
}

// 同一路径内容替换时保留行身份, 第二张表失败时整体回滚.
#[tokio::test]
async fn upsert_replaces_path_identity_and_rolls_back_on_path_failure() {
    // 覆盖首次插入, 重复更新, 内容替换及第二张表失败后的事务回滚.
    let root = std::env::temp_dir().join(format!("anime-upsert-{}", crate::utils::random_id().unwrap()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("episode.part.MKV");
    std::fs::write(&path, b"original").unwrap();
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../../../sql/init.sql")).execute(&pool).await.unwrap();
    // 新地址表只保存完整地址和指纹, 禁止旧的拆分字段及相对地址.
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('file_path') ORDER BY cid")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(columns, vec!["path", "pre16mib_md5"]);
    let first = ScannedFile::new(&path).unwrap();
    let old_id = first.pre16mib_md5();
    upsert_file(&pool, &first).await.unwrap();
    assert!(sqlx::query("INSERT INTO file_path(path, pre16mib_md5) VALUES ('relative.mkv', ?)")
        .bind(old_id).execute(&pool).await.is_err());
    save_progress(&pool, old_id, 12.5).await.unwrap();
    save_track_preferences(&pool, old_id, Some(Some(1)), Some(Some(3))).await.unwrap();
    set_mock_identification(&pool, old_id).await.unwrap();
    let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM file_path").fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE file SET size = 0").execute(&pool).await.unwrap();
    upsert_file(&pool, &first).await.unwrap();
    let video = get_video_by_file_id(&pool, old_id).await.unwrap().unwrap();
    assert_eq!(video.size, first.size());
    assert_eq!(video.watch_pos, Some(12.5));
    assert_eq!(video.audio_stream_index, Some(1));
    assert_eq!(video.subtitle_stream_index, Some(3));
    assert_eq!(video.ddp_epi_id, Some(1));

    std::fs::write(&path, b"replacement").unwrap();
    let replacement = ScannedFile::new(&path).unwrap();
    upsert_file(&pool, &replacement).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT pre16mib_md5 FROM file_path")
        .fetch_one(&pool).await.unwrap(), replacement.pre16mib_md5());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT rowid FROM file_path").fetch_one(&pool).await.unwrap(), rowid);
    assert_eq!(get_progress(&pool, old_id).await.unwrap().unwrap().watch_pos, Some(12.5));
    assert_eq!(get_progress(&pool, replacement.pre16mib_md5()).await.unwrap().unwrap().audio_stream_index, None);

    sqlx::raw_sql("CREATE TRIGGER reject_path BEFORE UPDATE ON file_path BEGIN SELECT RAISE(ABORT, 'reject path'); END;")
        .execute(&pool).await.unwrap();
    std::fs::write(&path, b"rejected replacement").unwrap();
    let rejected = ScannedFile::new(&path).unwrap();
    assert!(upsert_file(&pool, &rejected).await.is_err());
    assert!(get_progress(&pool, rejected.pre16mib_md5()).await.unwrap().is_none());
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT pre16mib_md5 FROM file_path")
        .fetch_one(&pool).await.unwrap(), replacement.pre16mib_md5());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

// 删除单个副本路径时保留兄弟路径和共享业务数据.
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
        sqlx::raw_sql(include_str!("../../../sql/init.sql")).execute(&pool).await.unwrap();
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

// 非法地址删除必须报错且不影响已有记录.
#[tokio::test]
async fn invalid_path_deletion_preserves_existing_records() {
    // 无效地址删除失败不影响现有记录, 写入回滚由独立 upsert 测试覆盖.
    let root = std::env::temp_dir().join(format!("anime-update-rollback-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let old = root.join("old.mkv");
    std::fs::write(&old, b"old").unwrap();
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../../../sql/init.sql")).execute(&pool).await.unwrap();
    upsert_file(&pool, &ScannedFile::new(&old).unwrap()).await.unwrap();
    assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
    // 无效地址不得误删同目录的有效文件.
    for invalid in ["/", "relative/video.mkv", "/media/../video.mkv"] {
        assert!(delete_path(&pool, Path::new(invalid)).await.is_err());
        assert_eq!(get_paths(&pool).await.unwrap(), vec![old.clone()]);
    }
    std::fs::remove_dir_all(&root).unwrap();
}
