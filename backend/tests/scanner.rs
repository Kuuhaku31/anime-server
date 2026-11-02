
// backend/tests/scanner.rs
//! 验证前缀重新哈希, 失效路径清理和媒体目录边界.

use super::*;
use crate::testing::TestDirectory;

/// 全量扫描必须重算身份, 保持同内容副本并移除消失路径.
#[tokio::test]
async fn scanning_updates_changed_content_and_deletes_only_missing_paths() -> Result<()> {
    let directory = TestDirectory::new();
    let root = directory.path.join("media");
    tokio::fs::create_dir(&root).await?;
    let first = root.join("episode.MKV");
    let copy = root.join("copy.mkv");
    tokio::fs::write(&first, b"first media").await?;
    tokio::fs::copy(&first, &copy).await?;
    let database = DatabaseManager::initialize(&directory.path.join("main.db")).await?;
    let scanner = ScannerManager::new(&root).await?;
    let changes = scanner.scan_all(&database).await?;
    assert_eq!((changes.added, changes.updated, changes.total), (2, 0, 2));
    let (first_media_id, _) = database.get_file_by_path(&first).await?.unwrap();
    let (copy_media_id, _) = database.get_file_by_path(&copy).await?.unwrap();
    assert_eq!(copy_media_id, first_media_id);
    tokio::fs::write(&first, b"other media").await?;
    let changes = scanner.scan_all(&database).await?;
    assert_eq!((changes.updated, changes.total), (1, 2));
    let (updated_media_id, _) = database.get_file_by_path(&first).await?.unwrap();
    assert_ne!(updated_media_id, first_media_id);
    tokio::fs::remove_file(&copy).await?;
    assert!(scanner.scan_one(&database, &copy).await?.is_none());
    assert!(database.get_file_by_path(&copy).await?.is_none());
    database.shutdown().await;
    Ok(())
}

/// 符号链接和父目录路径不得绕过媒体根目录限制.
#[tokio::test]
async fn media_paths_cannot_escape_the_root() -> Result<()> {
    let directory = TestDirectory::new();
    let root = directory.path.join("media");
    tokio::fs::create_dir(&root).await?;
    let outside = directory.path.join("outside.mkv");
    tokio::fs::write(&outside, b"not in the media root").await?;
    std::os::unix::fs::symlink(&outside, root.join("linked.mkv"))?;
    let scanner = ScannerManager::new(&root).await?;
    assert!(scanner.resolve(&root.join("linked.mkv")).await.is_err());
    assert!(scanner.check_path(&root.join("../outside.mkv")).is_err());
    assert!(scanner.check_path(Path::new("episode.mkv")).is_err());
    Ok(())
}

/// 全量扫描中原地址换内容且旧内容移到新地址时, 旧内容的共享配置不能丢失.
#[tokio::test]
async fn moving_media_and_reusing_its_old_path_preserves_configs() -> Result<()> {
    let directory = TestDirectory::new();
    let root = directory.path.join("media");
    tokio::fs::create_dir(&root).await?;
    let first = root.join("a.mkv");
    let moved = root.join("z.mkv");
    tokio::fs::write(&first, b"original media").await?;
    let database = DatabaseManager::initialize(&directory.path.join("main.db")).await?;
    let scanner = ScannerManager::new(&root).await?;
    scanner.scan_all(&database).await?;
    let configs = crate::models::MediaConfigs { position: Some(7.375), ..Default::default() };
    database.save_configs_by_path(&first, &configs).await?;
    tokio::fs::rename(&first, &moved).await?;
    tokio::fs::write(&first, b"different media").await?;
    scanner.scan_all(&database).await?;
    let (_, moved_configs) = database.get_file_by_path(&moved).await?.unwrap();
    let (_, replacement_configs) = database.get_file_by_path(&first).await?.unwrap();
    assert_eq!(moved_configs, configs);
    assert_eq!(replacement_configs.position, None);
    database.shutdown().await;
    Ok(())
}
