//! 校验缓存状态机, 生命周期, 磁盘淘汰及发布监控的测试.

use std::{path::PathBuf, sync::Arc, time::{Duration, Instant, SystemTime}};

use tokio::{io::AsyncReadExt, sync::{watch, Mutex, Notify}};

use super::{persist_used, testing, CacheFolder, CacheManager, CachePhase, FolderPhase, FolderState};
use crate::{
    config::PlaybackConfig,
    models::PlaybackVariantConfig,
    process::{testing::{fixture, published_media, FILE_ID}, CachedMedia, EncodeSpec, ProcessManager},
};

// 状态序列化名称属于会话接口契约.
#[test]
fn cache_phases_serialize_as_snake_case() {
    let phases = [
        (CachePhase::Loading, "loading"),
        (CachePhase::Ready, "ready"),
        (CachePhase::Running, "running"),
        (CachePhase::Stopping, "stopping"),
        (CachePhase::Complete, "complete"),
        (CachePhase::Failed, "failed"),
    ];
    for (phase, expected) in phases {
        assert_eq!(serde_json::to_value(phase).unwrap(), expected);
    }
}

// 内部状态机携带的控制数据不能泄露到公开阶段.
#[test]
fn folder_phases_map_to_public_cache_phases() {
    let (cancel, _) = watch::channel(false);
    let phases = [
        (FolderPhase::Loading { cancel: cancel.clone() }, CachePhase::Loading),
        (FolderPhase::Ready, CachePhase::Ready),
        (FolderPhase::Running {
            cancel: cancel.clone(), desired_threads: 2, encoding_threads: 0,
            pause_after: None, deadline: None,
        }, CachePhase::Running),
        (FolderPhase::Stopping {
            cancel, loading: false, stop_after: None, deadline: None,
        }, CachePhase::Stopping),
        (FolderPhase::Complete, CachePhase::Complete),
        (FolderPhase::Failed, CachePhase::Failed),
    ];
    for (phase, expected) in phases {
        assert_eq!(phase.snapshot(), expected);
    }
}

// 快照只包含会话所需字段, 默认空媒体的缓冲末端为 0.
#[tokio::test]
async fn cache_snapshot_exposes_only_session_status_fields() {
    let folder = folder();
    let state = folder.state.lock().await;
    let snapshot = folder.snapshot(&state);
    assert_eq!(snapshot.phase(), CachePhase::Ready);
    assert_eq!(snapshot.revision(), 1);
    assert_eq!(snapshot.buffered_until(), 0.0);
    assert_eq!(snapshot.duration(), folder.spec.duration);
}

// 已返回的快照是值拷贝, 后续状态更新不影响调用方持有的结果.
#[tokio::test]
async fn folder_snapshot_is_independent_of_later_updates() {
    let folder = folder();
    {
        let mut state = folder.state.lock().await;
        state.media = published_media(2, true);
        state.phase = FolderPhase::Complete;
        state.revision = 42;
    }
    let snapshot = folder.snapshot(&*folder.state.lock().await);
    assert_eq!(snapshot.phase(), CachePhase::Complete);
    assert_eq!(snapshot.revision(), 42);
    assert_eq!(snapshot.buffered_until(), 4.0);
    assert_eq!(snapshot.duration(), 20.0);
    {
        let mut state = folder.state.lock().await;
        state.media = CachedMedia::default();
        state.phase = FolderPhase::Failed;
        state.revision = 43;
    }
    let updated = folder.snapshot(&*folder.state.lock().await);
    assert_eq!(updated.phase(), CachePhase::Failed);
    assert_eq!(updated.revision(), 43);
    assert_eq!(updated.buffered_until(), 0.0);
    assert_eq!(snapshot.phase(), CachePhase::Complete);
    assert_eq!(snapshot.buffered_until(), 4.0);
}

// status 只读取活动 Map, 条目移除后不能再通过 variant-key 访问.
#[tokio::test]
async fn manager_status_reads_only_active_folders() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    let folder = Arc::new(folder());
    let variant_key = folder.variant_key.clone();
    folder.state.lock().await.revision = 42;
    manager.inner.entries.lock().await.insert(variant_key.clone(), folder);
    let status = manager.status(&variant_key).await.unwrap();
    assert_eq!(status.revision(), 42);
    assert_eq!(status.duration(), 20.0);
    manager.inner.entries.lock().await.remove(&variant_key);
    assert!(manager.status(&variant_key).await.is_err());
    assert!(manager.media_path(&variant_key, "init.mp4").await.is_err());
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 停止条目直接拒绝, 正常条目仍可复用, 两者都不依赖状态通知.
#[tokio::test]
async fn acquire_rejects_stopping_and_reuses_ready_folder() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    let folder = Arc::new(folder());
    let config = folder.spec.config.clone();
    let variant_key = folder.variant_key.clone();
    folder.state.lock().await.phase = FolderPhase::Stopping {
        cancel: watch::channel(false).0,
        loading: false, stop_after: None, deadline: None,
    };
    manager.inner.entries.lock().await.insert(variant_key.clone(), folder.clone());
    let error = tokio::time::timeout(Duration::from_secs(1), manager.acquire(&config))
        .await.unwrap().err().unwrap();
    assert_eq!(error.to_string(), "playback cache is stopping");
    assert!(Arc::ptr_eq(&manager.get(&variant_key).await.unwrap(), &folder));
    folder.state.lock().await.phase = FolderPhase::Ready;
    assert_eq!(manager.acquire(&config).await.unwrap().phase(), CachePhase::Ready);
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 在锁外查询期间插入胜出条目, 验证失败请求不会替换条目或启动恢复任务.
#[tokio::test]
async fn acquire_reports_concurrent_activation_without_replacing_folder() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    testing::register_media(&manager, FILE_ID).await;
    let folder = Arc::new(folder());
    let config = folder.spec.config.clone();
    let variant_key = folder.variant_key.clone();
    let metadata = manager.inner.metadata.lock().await;
    let acquiring = manager.acquire(&config);
    tokio::pin!(acquiring);
    // 锁住元数据使 acquire 必然停在初次 Map 未命中之后, 无需调度时序猜测.
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut acquiring).await.is_err());
    manager.inner.entries.lock().await.insert(variant_key.clone(), folder.clone());
    drop(metadata);
    let error = tokio::time::timeout(Duration::from_secs(1), &mut acquiring)
        .await.unwrap().err().unwrap();
    assert_eq!(error.to_string(), "playback cache was activated by another request");
    assert!(Arc::ptr_eq(&manager.get(&variant_key).await.unwrap(), &folder));
    assert!(manager.inner.workers.lock().await.is_empty());
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 启动不把磁盘目录加入活动 Map, 完整与部分缓存都参与 LRU, 未知条目保持不动.
#[tokio::test]
async fn startup_does_not_activate_disk_entries_and_lru_includes_partial_caches() {
    let config = PlaybackConfig { cache_max_bytes: 80, ..PlaybackConfig::default() };
    let (manager, root) = manager_fixture(config).await;
    let playback = root.join("playback");
    for (name, age) in [('a', 1), ('b', 2)] {
        let directory = playback.join(name.to_string().repeat(32));
        tokio::fs::create_dir(&directory).await.unwrap();
        tokio::fs::write(directory.join("segment_000000.m4s"), vec![0; 60]).await.unwrap();
        if name == 'b' { tokio::fs::write(directory.join("complete"), []).await.unwrap(); }
        persist_used(&directory, SystemTime::UNIX_EPOCH + Duration::from_secs(age)).await.unwrap();
    }
    tokio::fs::create_dir(playback.join("unknown")).await.unwrap();
    assert!(manager.inner.entries.lock().await.is_empty());
    manager.maintain().await.unwrap();
    assert!(!playback.join("a".repeat(32)).exists());
    assert!(playback.join("b".repeat(32)).exists());
    assert!(playback.join("unknown").exists());
    // 目录锁保证同一缓存根目录只能被一个管理器使用.
    let database = sqlx::sqlite::SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
    assert!(CacheManager::new(root.clone(), PlaybackConfig::default(), database, ProcessManager::new(2, 16))
        .await.is_err());
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 最后租约移除活动条目, 仍被引用的目录受保护, 已打开的文件在淘汰后仍可读取.
#[tokio::test]
async fn last_lease_retires_the_entry_and_allows_eviction() {
    let config = PlaybackConfig { cache_max_bytes: 1, ..PlaybackConfig::default() };
    let (manager, root) = manager_fixture(config).await;
    let mut folder = folder();
    folder.directory = root.join("playback").join(&folder.variant_key);
    let folder = Arc::new(folder);
    let variant_key = folder.variant_key.clone();
    let directory = folder.directory.clone();
    tokio::fs::create_dir(&directory).await.unwrap();
    tokio::fs::write(directory.join("init.mp4"), b"file-open fixture").await.unwrap();
    tokio::fs::write(directory.join("segment_000000.m4s"), b"segment").await.unwrap();
    folder.state.lock().await.media = published_media(1, false);
    manager.inner.entries.lock().await.insert(variant_key.clone(), folder.clone());
    manager.inner.folders.lock().await.insert(variant_key.clone(), Arc::downgrade(&folder));
    manager.maintain().await.unwrap();
    assert!(directory.exists());
    assert!(manager.media_path(&variant_key, "segment_000001.m4s").await.is_err());
    assert!(manager.media_path(&variant_key, "../main.db").await.is_err());
    let path = manager.media_path(&variant_key, "init.mp4").await.unwrap();
    let mut file = tokio::fs::File::open(path).await.unwrap();
    manager.release(&variant_key).await.unwrap();
    manager.release(&variant_key).await.unwrap();
    assert!(manager.status(&variant_key).await.is_err());
    // 活动 Map 已移除但仍有强引用时, 淘汰必须等待引用释放.
    manager.maintain().await.unwrap();
    assert!(directory.exists());
    drop(folder);
    manager.maintain().await.unwrap();
    assert!(!directory.exists());
    let mut data = Vec::new();
    file.read_to_end(&mut data).await.unwrap();
    assert_eq!(data, b"file-open fixture");
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 释放正在编码的缓存时等下一分片边界停止, 未编码时立即取消, 加载中则标记加载停止.
#[tokio::test]
async fn release_moves_active_phases_to_stopping() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    let folder = Arc::new(folder());
    let config = folder.spec.config.clone();
    let variant_key = folder.variant_key.clone();
    let (cancel, receiver) = watch::channel(false);
    {
        let mut state = folder.state.lock().await;
        state.media = published_media(3, false);
        state.phase = FolderPhase::Running {
            cancel, desired_threads: 2, encoding_threads: 2, pause_after: None, deadline: None,
        };
    }
    manager.inner.entries.lock().await.insert(variant_key.clone(), folder.clone());
    manager.release(&variant_key).await.unwrap();
    {
        let state = folder.state.lock().await;
        let FolderPhase::Stopping { loading, stop_after, deadline, .. } = &state.phase
            else { panic!("running folder must stop") };
        assert!(!loading);
        assert_eq!(*stop_after, Some(4));
        assert!(deadline.unwrap() > Instant::now());
    }
    assert!(!*receiver.borrow());
    // 重复释放保持原停止边界, 停止期间拒绝复用和读取资源.
    manager.release(&variant_key).await.unwrap();
    assert!(matches!(folder.state.lock().await.phase, FolderPhase::Stopping { stop_after: Some(4), .. }));
    assert_eq!(manager.acquire(&config).await.err().unwrap().to_string(), "playback cache is stopping");
    assert!(manager.media_path(&variant_key, "init.mp4").await.is_err());

    let (cancel, receiver) = watch::channel(false);
    folder.state.lock().await.phase = FolderPhase::Running {
        cancel, desired_threads: 2, encoding_threads: 0, pause_after: None, deadline: None,
    };
    manager.release(&variant_key).await.unwrap();
    assert!(matches!(folder.state.lock().await.phase, FolderPhase::Stopping { stop_after: None, deadline: None, .. }));
    assert!(*receiver.borrow());

    folder.state.lock().await.phase = FolderPhase::Loading { cancel: watch::channel(false).0 };
    manager.release(&variant_key).await.unwrap();
    assert!(matches!(folder.state.lock().await.phase, FolderPhase::Stopping { loading: true, .. }));
    manager.inner.entries.lock().await.clear();
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 恢复阶段 I/O 失败后进入 Failed, 最后释放必须移除条目而不是留下无租约的活动缓存.
#[tokio::test]
async fn activation_io_failure_does_not_leave_an_unleased_active_entry() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    testing::register_media(&manager, FILE_ID).await;
    let config = folder().spec.config;
    // 用普通文件阻断目录创建, 同时触发恢复失败和释放时的访问时间写入失败.
    let playback = root.join("playback");
    tokio::fs::remove_dir(&playback).await.unwrap();
    tokio::fs::write(&playback, b"not a directory").await.unwrap();
    let snapshot = manager.acquire(&config).await.unwrap();
    assert_eq!(snapshot.phase(), CachePhase::Failed);
    assert!(manager.release(config.variant_key()).await.is_err());
    assert!(manager.status(config.variant_key()).await.is_err());
    tokio::fs::remove_file(&playback).await.unwrap();
    tokio::fs::create_dir(&playback).await.unwrap();
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

// 模拟提交持锁等待磁盘, 防止监控分支与同一 select 中的提交 future 自锁.
#[tokio::test]
async fn publication_monitor_never_waits_for_the_future_it_is_polling() {
    let (manager, root) = manager_fixture(PlaybackConfig::default()).await;
    let folder = folder();
    let (cancel, receiver) = watch::channel(false);
    folder.state.lock().await.phase = FolderPhase::Running {
        cancel, desired_threads: 2, encoding_threads: 2, pause_after: None, deadline: None,
    };
    let mut child = tokio::process::Command::new("sleep").arg("5").kill_on_drop(true).spawn().unwrap();
    let step = async {
        let _state = folder.state.lock().await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(42)
    };
    let result = tokio::time::timeout(Duration::from_secs(1),
        manager.checked_step(&folder, &mut child, &mut None, Instant::now(), 0, &receiver, step)).await;
    child.kill().await.unwrap();
    manager.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
    assert_eq!(result.unwrap().unwrap(), 42);
}

// ========================================================================== //

// 每个测试独占缓存目录和内存数据库, 避免访问真实媒体或持久缓存.
async fn manager_fixture(config: PlaybackConfig) -> (CacheManager, PathBuf) {
    testing::manager(config, ProcessManager::new(2, 16)).await
}

// 构造处于 Ready 阶段的空缓存文件夹, 不启动媒体进程或访问真实视频.
fn folder() -> CacheFolder {
    let (info, request) = fixture();
    let config = PlaybackVariantConfig::new(FILE_ID, &info, &request).unwrap();
    CacheFolder {
        variant_key: config.variant_key().to_owned(),
        directory: PathBuf::new(),
        spec: EncodeSpec::from_config(config, PathBuf::new(), info).unwrap(),
        state: Mutex::new(FolderState {
            media: CachedMedia::default(),
            phase: FolderPhase::Ready,
            revision: 1,
            last_used: SystemTime::now(),
        }),
        changed: Notify::new(),
    }
}
