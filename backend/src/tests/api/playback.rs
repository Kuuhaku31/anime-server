
// backend/src/tests/api/playback.rs
//! 验证直接选轨, null 关闭语义和最小播放请求.

use super::*;

/// 缺省继承配置, 显式 null 关闭, 三类流都按全局索引校验.
#[test]
fn selected_streams_preserve_defaults_and_explicit_disabled_choices() {
    let metadata = crate::testing::metadata();
    let configs = MediaConfigs { position: Some(7.375), video_stream_index: Some(4),
        audio_stream_index: Some(3), subtitle_stream_index: Some(2) };
    let query: PlaybackQuery = serde_json::from_value(serde_json::json!({"path": "/media/episode.mkv"})).unwrap();
    let spec = select_streams("a".repeat(32), &query, &metadata, &configs).unwrap();
    assert_eq!((spec.video_stream_index, spec.audio_stream_index, spec.subtitle_stream_index), (4, Some(3), Some(2)));
    let query: PlaybackQuery = serde_json::from_value(serde_json::json!({
        "path": "/media/episode.mkv", "video_stream_index": 0, "audio_stream_index": null, "subtitle_stream_index": null,
    })).unwrap();
    let spec = select_streams("a".repeat(32), &query, &metadata, &configs).unwrap();
    assert_eq!((spec.video_stream_index, spec.audio_stream_index, spec.subtitle_stream_index), (0, None, None));
}

/// 拒绝不存在或类别不匹配的索引, 不允许客户端传入等待位置和缓存身份.
#[test]
fn invalid_indices_and_removed_playback_fields_are_rejected() {
    let metadata = crate::testing::metadata();
    for field in ["video_stream_index", "audio_stream_index", "subtitle_stream_index"] {
        let query: PlaybackQuery = serde_json::from_value(serde_json::json!({
            "path": "/media/episode.mkv", field: 99,
        })).unwrap();
        assert_eq!(select_streams("a".repeat(32), &query, &metadata, &MediaConfigs::default())
            .unwrap_err().status, StatusCode::BAD_REQUEST);
    }

    for field in ["position", "media_id", "handle_id", "version"] {
        assert!(serde_json::from_value::<PlaybackQuery>(serde_json::json!({
            "path": "/media/episode.mkv", field: 1,
        })).is_err());
    }
}

/// 重试选择稳定排序的现存副本, 缺失源和身份变化均明确拒绝.
#[tokio::test]
async fn cache_sources_are_sorted_and_retries_reject_changed_content() -> anyhow::Result<()> {
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
    let error = prepare_media(&second, Some(&original.media_id), &database, &scanner,
        &ProcessManager::new(2, 2)).await.err().unwrap();
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error.message.contains("content changed"));
    assert_eq!(source_path(&original.media_id, &database, &scanner).await.unwrap(), None);
    let error = prepare_media(&first, Some(&original.media_id), &database, &scanner,
        &ProcessManager::new(2, 2)).await.err().unwrap();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    database.shutdown().await;
    Ok(())
}
