
// backend/tests/db.rs
//! 验证按路径共享配置和内容身份替换的事务行为.

use super::*;
use crate::testing::TestDirectory;

/// 同内容副本共享配置, 替换一个副本不会破坏另一副本的配置.
#[tokio::test]
async fn copies_share_configs_and_replaced_paths_get_the_correct_identity() -> Result<()> {
    let directory = TestDirectory::new();
    let database = DatabaseManager::initialize(&directory.path.join("main.db")).await?;
    let first = ScannedFile { media_id: "a".repeat(32), path: "/media/first.mkv".into() };
    let second = ScannedFile { media_id: first.media_id.clone(), path: "/media/copy.mkv".into() };
    database.replace_scanned_path(&first).await?;
    database.replace_scanned_path(&second).await?;
    let configs = MediaConfigs { position: Some(7.375), video_stream_index: Some(4),
        audio_stream_index: None, subtitle_stream_index: Some(2) };
    database.save_configs_by_path(&first.path, &configs).await?;
    assert_eq!(database.get_file_by_path(&second.path).await?, Some((first.media_id.clone(), configs.clone())));
    let replacement = ScannedFile { media_id: "b".repeat(32), path: first.path.clone() };
    database.replace_scanned_path(&replacement).await?;
    assert_eq!(database.get_file_by_path(&first.path).await?, Some((replacement.media_id, MediaConfigs::default())));
    assert_eq!(database.get_file_by_path(&second.path).await?, Some((first.media_id.clone(), configs.clone())));
    let listed = database.list_paths_with_configs().await?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], (first.media_id.clone(), PathBuf::from("/media/copy.mkv"), configs.clone()));
    assert_eq!(database.paths_with_configs_by_id(&first.media_id).await?,
        vec![(PathBuf::from("/media/copy.mkv"), configs.clone())]);
    assert!(database.paths_with_configs_by_id(&"c".repeat(32)).await?.is_empty());
    database.delete_scanned_paths(std::slice::from_ref(&second.path)).await?;
    assert!(database.get_file_by_path(&second.path).await?.is_none());
    let remaining_paths = database.list_paths().await?;
    assert_eq!(remaining_paths.len(), 1);
    assert_eq!(remaining_paths[0].0, first.path);
    let saved_position: Option<f64> = sqlx::query_scalar("SELECT position FROM file WHERE media_id = ?")
        .bind(&first.media_id).fetch_one(&database.pool).await?;
    assert_eq!(saved_position, configs.position);
    assert!(database.save_configs_by_path(Path::new("/media/missing.mkv"), &configs).await.is_err());
    database.shutdown().await;
    Ok(())
}

/// 旧 schema 必须报错, 不能悄悄保留兼容字段或删除用户数据.
#[tokio::test]
async fn old_database_schema_is_rejected_without_destroying_records() -> Result<()> {
    let directory = TestDirectory::new();
    let path = directory.path.join("main.db");
    let options = SqliteConnectOptions::new().filename(&path).create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::query("CREATE TABLE file (pre16mib_md5 TEXT PRIMARY KEY)").execute(&pool).await?;
    pool.close().await;
    let error = DatabaseManager::initialize(&path).await.err().unwrap();
    assert!(error.to_string().contains("incompatible database schema"));
    assert!(path.exists());
    Ok(())
}
