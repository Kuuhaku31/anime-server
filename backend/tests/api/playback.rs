
// backend/tests/api/playback.rs
// 验证 API 请求和响应契约以及缓存源路径查询.

use super::*;

/// HTTP JSON 提取器按当前契约接收重扫路径字符串和配置对象.
#[tokio::test]
async fn media_mutation_requests_match_frontend_json() {
    use axum::extract::FromRequest;

    let request = Request::builder().header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#""/media/episode.mkv""#)).unwrap();
    let Json(path) = Json::<PathBuf>::from_request(request, &()).await.unwrap();
    assert_eq!(path, PathBuf::from("/media/episode.mkv"));

    let request = Request::builder().header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({
            "path": "/media/episode.mkv",
            "configs": {
                "position": 12.5,
                "video_stream_index": 0,
                "audio_stream_index": null,
                "subtitle_stream_index": null
            }
        }).to_string())).unwrap();
    let Json(input) = Json::<SaveMediaConfigsRequest>::from_request(request, &()).await.unwrap();
    assert_eq!(input.path, path);
    assert_eq!(input.configs.position, Some(12.5));
}

/// metadata 实际响应保持对象字段并隐藏内部探测信息.
#[test]
fn media_response_keeps_internal_probe_details_private() {
    let response = MediaMetadataResponseBody {
        metadata: crate::testing::metadata(),
        configs: MediaConfigs::default(),
    };
    let json = serde_json::to_value(response).unwrap();
    assert!(json["configs"].is_object());
    assert!(json["metadata"]["video_streams"].is_array());
    assert!(json["metadata"].get("start_time").is_none());
    assert!(json["metadata"].get("attachments").is_none());
    assert!(json["metadata"]["video_streams"][0].get("color_transfer").is_none());
}

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

/// 各缓存响应共用扁平快照, 列表和构建仍保留各自的对象封装.
#[test]
fn cache_responses_share_flat_snapshots() {
    let response = CacheListResponseBody { entries: Vec::new() };
    assert_eq!(serde_json::to_value(response).unwrap(), serde_json::json!({ "entries": [] }));
    let spec = crate::testing::spec();
    let snapshot = CacheSnapshot {
        handle_id: spec.handle_id().unwrap(), spec,
        phase: "failed".to_string(), buffered_until: 12.5, error: Some("encoding failed".to_string()),
        resources: process::WorkerResources::default(),
        source_path: Some(PathBuf::from("/media/episode.mkv")),
    };
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["phase"], "failed");
    assert_eq!(json["buffered_until"], 12.5);
    assert_eq!(json["error"], "encoding failed");
    assert!(json.get("status").is_none());
    let list = CacheListResponseBody { entries: vec![snapshot.clone()] };
    assert_eq!(serde_json::to_value(list).unwrap()["entries"][0], json);
    let build = BuildCacheResponseBody {
        cache: snapshot, media_meta: crate::testing::metadata(), media_configs: MediaConfigs::default(),
    };
    assert_eq!(serde_json::to_value(build).unwrap()["cache"], json);
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
