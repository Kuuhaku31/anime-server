
// backend/src/tests/cache.rs
//! 验证全量句柄映射, 并发生成, 停止和持久缓存恢复.

use super::*;
use std::path::Path;
use crate::{models::ScannedFile, testing::{create_media, metadata, spec, wait_phase, TestDirectory}};

/// 同一身份的并发请求只建立一个目录, 完成缓存可在没有源文件时恢复.
#[tokio::test]
async fn concurrent_opens_share_one_handle_and_completed_cache_survives_restart() -> Result<()> {
    let directory = TestDirectory::new();
    let source = directory.path.join("source.mkv");
    create_media(&source).await?;
    let processes = ProcessManager::new(2, 2);
    let metadata = processes.probe(&source).await?;
    let mut spec = spec();
    spec.media_id = ScannedFile::new(&source)?.media_id;
    let config = AppConfigManager { ffmpeg_threads: 1, ..AppConfigManager::default() };
    let root = directory.path.join("cache");
    let manager = CacheManager::new(root.clone(), &processes).await?;
    let (first, second) = tokio::join!(
        manager.build(spec.clone(), source.clone(), metadata.clone(), &config, &processes),
        manager.build(spec.clone(), source.clone(), metadata, &config, &processes),
    );
    let first = first?;
    assert_eq!(first.handle_id, second?.handle_id);
    let folder = root.join("playback").join(&first.handle_id);
    assert_eq!(manager.handles.lock().await.len(), 1);
    wait_phase(&manager, &first.handle_id, "completed").await?;
    let descriptor = tokio::fs::read(folder.join("instance.json")).await?;
    assert_eq!(CacheSpec::from_descriptor(&descriptor)?, spec);
    let published = tokio::fs::read(folder.join("segment_000000.m4s")).await?;
    assert!(manager.media_path(&first.handle_id, "instance.json").await.is_err());
    assert!(manager.media_path(&first.handle_id, "../init.mp4").await.is_err());
    let json = serde_json::to_value(manager.status(&first.handle_id).await?)?;
    assert_eq!(json["handle_id"], first.handle_id);
    assert!(json.get("folder").is_none());
    assert_eq!(json["resources"]["ffmpeg_threads"], 0);
    manager.shutdown().await?;
    drop(manager);
    tokio::fs::remove_file(&source).await?;
    let restored = CacheManager::new(root, &processes).await?;
    wait_phase(&restored, &first.handle_id, "completed").await?;
    assert_eq!(restored.handles.lock().await.len(), 1);
    assert_eq!(tokio::fs::read(folder.join("segment_000000.m4s")).await?, published);
    assert!(restored.status(&first.handle_id).await?.status.buffered_until > 8.9);
    restored.remove(&first.handle_id).await?;
    assert!(!folder.exists());
    assert!(restored.media_path(&first.handle_id, "index.m3u8").await.is_err());
    restored.shutdown().await?;
    Ok(())
}

/// 失败重试重新进入 Loading, 恢复后的编码仍复用原句柄和实例描述.
#[tokio::test]
async fn failed_generation_reloads_the_same_handle_on_retry() -> Result<()> {
    let directory = TestDirectory::new();
    let source = directory.path.join("source.mkv");
    create_media(&source).await?;
    let processes = ProcessManager::new(2, 2);
    let metadata = processes.probe(&source).await?;
    let mut spec = spec();
    spec.media_id = ScannedFile::new(&source)?.media_id;
    let backup = directory.path.join("source.hidden");
    tokio::fs::rename(&source, &backup).await?;
    let manager = CacheManager::new(directory.path.join("cache"), &processes).await?;
    let config = AppConfigManager { ffmpeg_threads: 1, ..AppConfigManager::default() };
    let first = manager.build(spec.clone(), source.clone(), metadata.clone(), &config, &processes).await?;
    wait_phase(&manager, &first.handle_id, "failed").await?;
    let handle = manager.handle(&first.handle_id).await?;
    assert_eq!(manager.status(&first.handle_id).await?.resources, WorkerResources::default());
    tokio::fs::rename(&backup, &source).await?;
    let second = manager.retry(&first.handle_id, source, metadata, &config, &processes).await?;
    assert!(Arc::ptr_eq(&handle, &manager.handle(&second.handle_id).await?));
    wait_phase(&manager, &second.handle_id, "completed").await?;
    manager.shutdown().await?;
    Ok(())
}

/// 排队编码和紧邻构建的删除都不能在 End 后重新创建目录.
#[tokio::test]
async fn remove_cancels_queued_generation_and_prevents_late_writers() -> Result<()> {
    let directory = TestDirectory::new();
    let processes = ProcessManager::new(2, 2);
    let (_sender, mut cancel) = tokio::sync::watch::channel(false);
    let _threads = processes.encoding_budget(2, 2, &mut cancel).await?;
    let manager = CacheManager::new(directory.path.join("cache"), &processes).await?;
    let config = AppConfigManager::default();
    for iteration in 0..4 {
        let mut spec = spec();
        spec.media_id = format!("{iteration:032x}");
        let snapshot = manager.build(spec, directory.path.join("missing.mkv"), metadata(), &config, &processes).await?;
        if iteration == 0 { wait_phase(&manager, &snapshot.handle_id, "running").await?; }
        assert_eq!(manager.status(&snapshot.handle_id).await?.resources, WorkerResources::default());
        let folder = manager.handle(&snapshot.handle_id).await?.path.clone();
        manager.remove(&snapshot.handle_id).await?;
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(!folder.exists());
        assert_eq!(manager.handles.lock().await.len(), 0);
    }
    Ok(())
}

/// 无效身份和未知目录都必须保留原始文件并明确阻止启动.
#[tokio::test]
async fn discovery_preserves_invalid_instances_and_unknown_entries() -> Result<()> {
    let directory = TestDirectory::new();
    let root = directory.path.join("cache");
    let invalid = root.join("playback").join("a".repeat(32));
    tokio::fs::create_dir_all(&invalid).await?;
    tokio::fs::write(invalid.join("instance.json"), b"invalid JSON").await?;
    let processes = ProcessManager::new(2, 2);
    assert!(CacheManager::new(root.clone(), &processes).await.is_err());
    assert_eq!(tokio::fs::read(invalid.join("instance.json")).await?, b"invalid JSON");
    tokio::fs::rename(&invalid, directory.path.join("preserved-invalid")).await?;
    let unknown = root.join("playback").join("user-directory");
    tokio::fs::create_dir(&unknown).await?;
    assert!(CacheManager::new(root, &processes).await.is_err());
    assert!(unknown.exists());
    Ok(())
}

/// 停止和关闭都保留部分缓存, 重启后只续接缺失尾部且不改变已发布前缀.
#[tokio::test]
async fn recovered_partial_cache_appends_without_overwriting_the_prefix() -> Result<()> {
    let directory = TestDirectory::new();
    let source = directory.path.join("source.mkv");
    create_media(&source).await?;
    let processes = ProcessManager::new(2, 2);
    let metadata = processes.probe(&source).await?;
    let mut spec = spec();
    spec.media_id = ScannedFile::new(&source)?.media_id;
    let config = AppConfigManager { ffmpeg_threads: 1, ..AppConfigManager::default() };
    let root = directory.path.join("cache");
    let manager = CacheManager::new(root.clone(), &processes).await?;
    let snapshot = manager.build(spec, source.clone(), metadata.clone(), &config, &processes).await?;
    wait_phase(&manager, &snapshot.handle_id, "completed").await?;
    let folder = root.join("playback").join(&snapshot.handle_id);
    let first = tokio::fs::read(folder.join("segment_000000.m4s")).await?;
    let second = tokio::fs::read(folder.join("segment_000001.m4s")).await?;
    let frames = frame_count(&processes, &folder).await?;
    manager.shutdown().await?;
    drop(manager);
    tokio::fs::write(folder.join("segment_000002.m4s"), b"broken tail").await?;
    let manager = CacheManager::new(root.clone(), &processes).await?;
    wait_phase(&manager, &snapshot.handle_id, "ready").await?;
    assert_eq!(manager.status(&snapshot.handle_id).await?.status.buffered_until, 8.0);
    let (_sender, mut cancel) = tokio::sync::watch::channel(false);
    let occupied = processes.encoding_budget(2, 2, &mut cancel).await?;
    manager.retry(&snapshot.handle_id, source.clone(), metadata.clone(), &config, &processes).await?;
    let stopped = manager.stop(&snapshot.handle_id).await?;
    assert_eq!(stopped.status.phase, "ready");
    assert_eq!(stopped.status.buffered_until, 8.0);
    assert_eq!(stopped.resources, WorkerResources::default());
    assert!(folder.exists());
    manager.retry(&snapshot.handle_id, source.clone(), metadata.clone(), &config, &processes).await?;
    manager.shutdown().await?;
    assert_eq!(tokio::fs::read(folder.join("segment_000000.m4s")).await?, first);
    assert_eq!(tokio::fs::read(folder.join("segment_000001.m4s")).await?, second);
    assert!(folder.join("instance.json").exists());
    assert_eq!(manager.status(&snapshot.handle_id).await?.resources, WorkerResources::default());
    drop(manager);
    let manager = CacheManager::new(root, &processes).await?;
    wait_phase(&manager, &snapshot.handle_id, "ready").await?;
    assert_eq!(manager.status(&snapshot.handle_id).await?.status.buffered_until, 8.0);
    drop(occupied);
    manager.retry(&snapshot.handle_id, source, metadata, &config, &processes).await?;
    wait_phase(&manager, &snapshot.handle_id, "completed").await?;
    assert_eq!(tokio::fs::read(folder.join("segment_000000.m4s")).await?, first);
    assert_eq!(tokio::fs::read(folder.join("segment_000001.m4s")).await?, second);
    assert_eq!(frame_count(&processes, &folder).await?, frames);
    manager.shutdown().await?;
    Ok(())
}

/// 列表按标识排序, 非法标识和已经删除的重试不会创建缓存.
#[tokio::test]
async fn handle_queries_are_canonical_sorted_and_do_not_create_caches() -> Result<()> {
    let directory = TestDirectory::new();
    let processes = ProcessManager::new(2, 2);
    let manager = CacheManager::new(directory.path.join("cache"), &processes).await?;
    for video_stream_index in [4, 0] {
        let mut spec = spec();
        spec.video_stream_index = video_stream_index;
        let handle_id = spec.handle_id()?;
        let handle = manager.create_handle(spec, handle_id.clone())?;
        manager.handles.lock().await.insert(handle_id, handle);
    }
    let entries = manager.list().await?;
    assert_eq!(entries.len(), 2);
    assert!(entries[0].handle_id < entries[1].handle_id);
    for invalid in ["../playback", "/cache", "AABBCCDDEEFF00112233445566778899", "123"] {
        assert!(manager.status(invalid).await.is_err());
        assert!(manager.remove(invalid).await.is_err());
        assert!(manager.media_path(invalid, "index.m3u8").await.is_err());
    }
    let handle_id = &entries[0].handle_id;
    manager.remove(handle_id).await?;
    let error = manager.retry(handle_id, directory.path.join("missing.mkv"), metadata(),
        &AppConfigManager::default(), &processes).await.unwrap_err();
    assert_eq!(error.downcast_ref::<CacheError>(), Some(&CacheError::NotFound));
    assert_eq!(manager.list().await?.len(), 1);
    manager.shutdown().await?;
    Ok(())
}

/// 真实读取整个 HLS 的视频帧数, 避免只用清单时长代替续接正确性.
async fn frame_count(processes: &ProcessManager, folder: &Path) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new("ffprobe");
    command.args(["-v", "error", "-count_frames", "-select_streams", "v:0",
        "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"]).arg(folder.join("index.m3u8"));
    processes.output(&mut command, 30).await
}
