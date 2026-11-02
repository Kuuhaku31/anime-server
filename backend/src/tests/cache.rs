
// backend/src/tests/cache.rs
//! 验证全量句柄映射, 并发生成, 停止和持久缓存恢复.

use super::*;
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
        manager.open(spec.clone(), source.clone(), metadata.clone(), &config, &processes),
        manager.open(spec.clone(), source.clone(), metadata, &config, &processes),
    );
    let first = first?;
    assert_eq!(first.folder, second?.folder);
    assert_eq!(manager.handles.lock().await.len(), 1);
    wait_phase(&manager, &first.folder, "completed").await?;
    let descriptor = tokio::fs::read(first.folder.join("instance.json")).await?;
    assert_eq!(CacheSpec::from_descriptor(&descriptor)?, spec);
    let published = tokio::fs::read(first.folder.join("segment_000000.m4s")).await?;
    assert!(manager.media_path(&first.folder, "instance.json").await.is_err());
    manager.shutdown().await?;
    drop(manager);
    tokio::fs::remove_file(&source).await?;
    let restored = CacheManager::new(root, &processes).await?;
    wait_phase(&restored, &first.folder, "completed").await?;
    assert_eq!(restored.handles.lock().await.len(), 1);
    assert_eq!(tokio::fs::read(first.folder.join("segment_000000.m4s")).await?, published);
    assert!(restored.status(&first.folder).await?.buffered_until > 8.9);
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
    let first = manager.open(spec.clone(), source.clone(), metadata.clone(), &config, &processes).await?;
    wait_phase(&manager, &first.folder, "failed").await?;
    let handle = manager.handle_by_path(&first.folder).await?;
    tokio::fs::rename(&backup, &source).await?;
    let second = manager.open(spec, source, metadata, &config, &processes).await?;
    assert!(Arc::ptr_eq(&handle, &manager.handle_by_path(&second.folder).await?));
    wait_phase(&manager, &second.folder, "completed").await?;
    manager.shutdown().await?;
    Ok(())
}

/// 排队编码和紧邻 open 的删除都不能在 End 后重新创建目录.
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
        let snapshot = manager.open(spec, directory.path.join("missing.mkv"), metadata(), &config, &processes).await?;
        if iteration == 0 { wait_phase(&manager, &snapshot.folder, "running").await?; }
        manager.remove(&snapshot.folder).await?;
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(!snapshot.folder.exists());
        assert_eq!(manager.handles.lock().await.len(), 0);
    }
    Ok(())
}

/// 启动可清理已知命名的无效实例, 但未知目录不应被自动删除.
#[tokio::test]
async fn discovery_removes_invalid_instances_but_preserves_unknown_entries() -> Result<()> {
    let directory = TestDirectory::new();
    let root = directory.path.join("cache");
    let invalid = root.join("playback").join("a".repeat(32));
    tokio::fs::create_dir_all(&invalid).await?;
    tokio::fs::write(invalid.join("instance.json"), b"invalid JSON").await?;
    let processes = ProcessManager::new(2, 2);
    let manager = CacheManager::new(root.clone(), &processes).await?;
    assert!(!invalid.exists());
    assert!(manager.handles.lock().await.is_empty());
    let unknown = root.join("playback").join("user-directory");
    tokio::fs::create_dir(&unknown).await?;
    assert!(CacheManager::new(root, &processes).await.is_err());
    assert!(unknown.exists());
    Ok(())
}

/// 损坏尾部恢复为 Ready 后只重编码缺失尾部, 已发布前缀和总帧数保持不变.
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
    let snapshot = manager.open(spec.clone(), source.clone(), metadata.clone(), &config, &processes).await?;
    wait_phase(&manager, &snapshot.folder, "completed").await?;
    let first = tokio::fs::read(snapshot.folder.join("segment_000000.m4s")).await?;
    let second = tokio::fs::read(snapshot.folder.join("segment_000001.m4s")).await?;
    let frames = frame_count(&processes, &snapshot.folder).await?;
    manager.shutdown().await?;
    drop(manager);
    tokio::fs::write(snapshot.folder.join("segment_000002.m4s"), b"broken tail").await?;
    let manager = CacheManager::new(root, &processes).await?;
    wait_phase(&manager, &snapshot.folder, "ready").await?;
    assert_eq!(manager.status(&snapshot.folder).await?.buffered_until, 8.0);
    manager.open(spec, source, metadata, &config, &processes).await?;
    wait_phase(&manager, &snapshot.folder, "completed").await?;
    assert_eq!(tokio::fs::read(snapshot.folder.join("segment_000000.m4s")).await?, first);
    assert_eq!(tokio::fs::read(snapshot.folder.join("segment_000001.m4s")).await?, second);
    assert_eq!(frame_count(&processes, &snapshot.folder).await?, frames);
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
