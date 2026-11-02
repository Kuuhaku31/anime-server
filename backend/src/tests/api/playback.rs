
// backend/src/tests/api/playback.rs
//! 验证播放请求字段和缓存源路径查询.

use super::*;

/// 请求字段缺省和显式 null 保持不同, 以区分继承偏好与关闭轨道.
#[test]
fn playback_query_distinguishes_missing_tracks_from_explicit_null() {
    let inherited: PlaybackQuery = serde_json::from_value(serde_json::json!({
        "path": "/media/episode.mkv",
    })).unwrap();
    assert_eq!(inherited.audio_stream_index, None);
    assert_eq!(inherited.subtitle_stream_index, None);

    let disabled: PlaybackQuery = serde_json::from_value(serde_json::json!({
        "path": "/media/episode.mkv",
        "audio_stream_index": null,
        "subtitle_stream_index": null,
    })).unwrap();
    assert_eq!(disabled.audio_stream_index, Some(None));
    assert_eq!(disabled.subtitle_stream_index, Some(None));

    let selected: PlaybackQuery = serde_json::from_value(serde_json::json!({
        "path": "/media/episode.mkv",
        "audio_stream_index": 3,
        "subtitle_stream_index": 2,
    })).unwrap();
    assert_eq!(selected.audio_stream_index, Some(Some(3)));
    assert_eq!(selected.subtitle_stream_index, Some(Some(2)));
}

/// 不允许客户端传入等待位置和缓存身份等非请求字段.
#[test]
fn removed_playback_fields_are_rejected() {
    for field in ["position", "media_id", "handle_id", "version"] {
        assert!(serde_json::from_value::<PlaybackQuery>(serde_json::json!({
            "path": "/media/episode.mkv", field: 1,
        })).is_err());
    }
}

/// 源查询使用稳定排序的现存副本, 文件缺失后回退到其他副本.
#[tokio::test]
async fn cache_sources_are_sorted_and_fall_back_to_existing_copies() -> anyhow::Result<()> {
    let directory = crate::testing::TestDirectory::new();
    let root = directory.path.join("media");
    tokio::fs::create_dir(&root).await?;
    let first = root.join("a.mkv");
    let second = root.join("b.mkv");
    tokio::fs::write(&first, b"source content").await?;
    tokio::fs::copy(&first, &second).await?;
    let database = DatabaseManager::initialize(&directory.path.join("main.db")).await?;
    let scanner = ScannerManager::new(&root).await?;
    let original = scanner.scan_one(&database, &first).await?.unwrap();
    scanner.scan_one(&database, &second).await?;
    assert_eq!(source_path(&original.media_id, &database, &scanner).await.unwrap(), Some(first.clone()));
    tokio::fs::remove_file(&first).await?;
    assert_eq!(source_path(&original.media_id, &database, &scanner).await.unwrap(), Some(second.clone()));
    tokio::fs::write(&second, b"different content").await?;
    scanner.scan_one(&database, &second).await?;
    assert_eq!(source_path(&original.media_id, &database, &scanner).await.unwrap(), None);
    assert_eq!(source_path("c".repeat(32).as_str(), &database, &scanner).await.unwrap(), None);
    database.shutdown().await;
    Ok(())
}
