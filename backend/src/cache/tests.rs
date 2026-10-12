//! 集中保存缓存状态, 生命周期及磁盘淘汰测试, 保持原有注释禁用状态.

use super::{CacheFolder, CachedMedia, FolderState};
use crate::{models::PlaybackVariantConfig, video::{self, EncodeSpec}};
use std::{path::PathBuf, time::SystemTime};
use tokio::sync::{watch, Mutex, Notify};

fn state() -> FolderState {
    let (cancel, _) = watch::channel(false);
    FolderState {
        media: CachedMedia::default(),
        loaded: true,
        active: true,
        worker_active: true,
        encoding: false,
        finalizing: false,
        encoding_threads: 0,
        desired_threads: 2,
        start_requested: false,
        stop_after: None,
        stop_deadline: None,
        cancel,
        revision: 1,
        last_used: SystemTime::now(),
        error: None,
    }
}

fn folder() -> CacheFolder {
    let (info, request) = video::fixture();
    let config = PlaybackVariantConfig::new(&"a".repeat(32), &info, &request).unwrap();
    CacheFolder {
        key: config.key().to_owned(),
        directory: PathBuf::new(),
        spec: EncodeSpec::from_config(config, PathBuf::new(), info).unwrap(),
        state: Mutex::new(state()),
        changed: Notify::new(),
    }
}

#[tokio::test]
async fn worker_waits_for_an_explicit_start_request() {
    let folder = folder();
    let mut cancel = folder.state.lock().await.cancel.subscribe();
    let waiting = folder.wait_start(&mut cancel);
    tokio::pin!(waiting);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting).await.is_err());

    folder.state.lock().await.request_start(8);
    folder.changed.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await.unwrap().unwrap();
}

#[test]
fn start_requests_can_change_before_encoding_and_are_ignored_afterward() {
    let mut state = state();

    assert!(state.request_start(8));
    assert!(state.request_start(4));
    assert_eq!(state.desired_threads, 4);
    assert!(state.start_requested);

    state.encoding = true;
    assert!(!state.request_start(8));
    assert_eq!(state.desired_threads, 4);

    state.encoding = false;
    state.media.complete = true;
    assert!(!state.request_start(8));
    assert_eq!(state.desired_threads, 4);
}

// use super::*;

// #[tokio::test]
// async fn folder_snapshot_preserves_status_and_is_independent_of_later_updates() {
//     let folder = folder();
//     let snapshot = folder.status().await.unwrap();
//     assert!(snapshot.complete());
//     assert!(snapshot.generating());
//     assert!(snapshot.stopping());
//     assert!(snapshot.ready());
//     assert_eq!(snapshot.buffered_until(), 4.0);
//     assert_eq!(snapshot.encoding_threads(), 2);
//     assert_eq!(snapshot.revision(), 42);
//     assert_eq!(snapshot.error(), Some("fixture error"));
//     assert!(snapshot.last_used > SystemTime::UNIX_EPOCH);
//
//     // 后续缓存更新不改变已返回快照中的媒体前缀和错误.
//     {
//         let mut state = folder.state.lock().await;
//         state.media = CachedMedia::default();
//         state.worker_active = false;
//         state.finalizing = false;
//         state.encoding_threads = 0;
//         state.error = None;
//     }
//     let updated = folder.status().await.unwrap();
//     assert!(!updated.complete() && !updated.generating() && !updated.stopping() && !updated.ready());
//     assert_eq!(updated.buffered_until(), 0.0);
//     assert_eq!(updated.encoding_threads(), 0);
//     assert_eq!(updated.error(), None);
//     assert!(snapshot.complete() && snapshot.ready());
//     assert_eq!(snapshot.buffered_until(), 4.0);
//     assert_eq!(snapshot.error(), Some("fixture error"));
//
//     folder.state.lock().await.stop_after = Some(2);
//     assert!(folder.status().await.unwrap().stopping());
//     folder.state.lock().await.active = false;
//     assert!(folder.status().await.is_err());
// }

// #[tokio::test]
// async fn manager_reads_duration_separately_from_folder_snapshot() {
//     let root = std::env::temp_dir().join(format!("anime-state-snapshot-{}", utils::random_id().unwrap()));
//     tokio::fs::create_dir(&root).await.unwrap();
//     let database = sqlx::sqlite::SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
//     let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), database, Processes::new(2, 16)).await.unwrap();
//     let folder = Arc::new(folder());
//     let key = folder.key.clone();
//     manager.inner.entries.lock().await.insert(key.clone(), folder);
//     assert_eq!(manager.duration(&key).await.unwrap(), 20.0);
//     assert_eq!(manager.status(&key).await.unwrap().revision(), 42);
//     manager.inner.entries.lock().await.remove(&key);
//     assert!(manager.duration(&key).await.is_err());
//     assert!(manager.status(&key).await.is_err());
//     drop(manager);
//     tokio::fs::remove_dir_all(root).await.unwrap();
// }

// // 构造已恢复的缓存文件夹, 不启动媒体进程或访问真实视频.
// fn folder() -> CacheFolder {
//     let (info, request) = video::fixture();
//     let config = PlaybackVariantConfig::new(&"a".repeat(32), &info, &request).unwrap();
//     CacheFolder {
//         key: config.key().to_owned(),
//         directory: PathBuf::new(),
//         spec: EncodeSpec::from_config(config, "video.mkv".into(), info).unwrap(),
//         state: Mutex::new(FolderState {
//             media: CachedMedia {
//                 complete: true, init: None,
//                 segments: vec![video::Fragment {
//                     start: Tail::default(), end: Tail { video: 360000, audio: 192000 }, sequence: 1,
//                 }],
//             },
//             loaded: true, active: true, worker_active: true, encoding: false,
//             finalizing: true, encoding_threads: 2, stop_after: None, stop_deadline: None,
//             cancel: watch::channel(false).0, revision: 42,
//             last_used: SystemTime::UNIX_EPOCH, error: Some("fixture error".to_owned()),
//         }),
//         changed: Notify::new(),
//     }
// }

// 以下保留原先注释掉的旧接口测试, 本次整理不恢复执行或更改测试逻辑.
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::video::models::PlaybackVariantConfig;
//
//     #[test]
//     fn configuration_serialization_is_canonical_and_excludes_runtime_fields() {
//         let config = PlaybackVariantConfig {
//             src_file_pre16mib_md5: "35af6d912b6dffadc112df2ca94bbe2f".to_string(),
//             video_stream: 0, audio_stream: Some(1), subtitle_stream: Some(3),
//             encoder_profile: "h264-high-yuv420p-aac-fmp4-v1".to_string(),
//         };
//         assert_eq!(config.key().unwrap(), "bc892e2d4aa401399639fe22ee897b43");
//         let mut updated = config.clone();
//         updated.encoder_profile = ENCODER_PROFILE.to_string();
//         assert_ne!(updated.key().unwrap(), config.key().unwrap());
//         updated.audio_stream = None;
//         updated.subtitle_stream = None;
//         assert!(String::from_utf8(serde_json::to_vec(&updated).unwrap()).unwrap()
//             .contains("\"audio_stream\":null,\"subtitle_stream\":null"));
//         for key in ["../main.db", "ABCDEF0123456789ABCDEF0123456789", "a", ""] {
//             assert!(!is_key(key));
//         }
//     }
//
//     #[tokio::test]
//     async fn startup_does_not_activate_disk_entries_and_lru_includes_partial_caches() {
//         // 不完整和完整目录都参与 LRU, 未知文件和目录保持不动.
//         let root = std::env::temp_dir().join(format!("anime-active-lru-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let config = PlaybackConfig { cache_max_bytes: 80, ..PlaybackConfig::default() };
//         let manager = CacheManager::new(root.clone(), config, Processes::new(2, 16)).await.unwrap();
//         for (name, age) in [('a', 1), ('b', 2)] {
//             let directory = root.join("playback").join(name.to_string().repeat(32));
//             tokio::fs::create_dir(&directory).await.unwrap();
//             tokio::fs::write(directory.join("segment_000000.m4s"), vec![0; 60]).await.unwrap();
//             if name == 'b' { tokio::fs::write(directory.join("complete"), []).await.unwrap(); }
//             persist_used(&directory, SystemTime::UNIX_EPOCH + Duration::from_secs(age)).await.unwrap();
//         }
//         assert!(manager.inner.entries.lock().await.is_empty());
//         manager.maintain().await.unwrap();
//         assert!(!root.join("playback").join("a".repeat(32)).exists());
//         assert!(root.join("playback").join("b".repeat(32)).exists());
//         assert!(CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(2, 16)).await.is_err());
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }
//
//     #[tokio::test]
//     async fn last_lease_retires_the_entry_but_an_open_pin_still_blocks_eviction() {
//         // 仅模拟已经发布的文件, 区分活动租约保护和退出集合后的文件打开保护.
//         use tokio::io::AsyncReadExt;
//         let root = std::env::temp_dir().join(format!("anime-media-pin-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let config = PlaybackConfig { cache_max_bytes: 1, ..PlaybackConfig::default() };
//         let manager = CacheManager::new(root.clone(), config, Processes::new(2, 16)).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let key = spec.config.key().unwrap();
//         let directory = root.join("playback").join(&key);
//         tokio::fs::create_dir(&directory).await.unwrap();
//         tokio::fs::write(directory.join("init.mp4"), b"file-open fixture").await.unwrap();
//         let output = Arc::new(ActiveCache {
//             key: key.clone(), directory: directory.clone(), spec: spec.clone(), changed: Notify::new(),
//             state: Mutex::new(ActiveState {
//                 media: CachedMedia { complete: true, init: None, segments: vec![fragment::Fragment {
//                     start: Tail::default(), end: Tail { video: 360000, audio: 192000 }, sequence: 1,
//                 }] },
//                 loaded: true, clients: HashMap::from([("first".to_string(), 0.0)]),
//                 worker_active: false, encoding: false, finalizing: false, encoding_threads: 0,
//                 stop_after: None, stop_deadline: None, cancel: watch::channel(false).0,
//                 revision: 1, last_used: SystemTime::now(), error: None,
//             }),
//         });
//         manager.inner.entries.lock().await.insert(key.clone(), output);
//         let shared = manager.acquire(spec, "second").await.unwrap();
//         assert_eq!(shared.state.lock().await.clients.len(), 2);
//         manager.maintain().await.unwrap();
//         assert!(directory.exists());
//         let (path, pin) = manager.media_path(&key, "second", "init.mp4").await.unwrap();
//         manager.release(&key, "first").await.unwrap();
//         manager.release(&key, "first").await.unwrap();
//         assert_eq!(shared.state.lock().await.clients.len(), 1);
//         drop(shared);
//         manager.release(&key, "second").await.unwrap();
//         assert!(manager.get(&key).await.is_err());
//         assert!(pin.status("second").await.is_err());
//         manager.maintain().await.unwrap();
//         assert!(directory.exists());
//         let mut file = tokio::fs::File::open(path).await.unwrap();
//         drop(pin);
//         manager.maintain().await.unwrap();
//         assert!(!directory.exists());
//         let mut data = Vec::new();
//         file.read_to_end(&mut data).await.unwrap();
//         assert_eq!(data, b"file-open fixture");
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }
//
//     #[tokio::test]
//     async fn stop_is_revocable_but_a_finalizing_writer_must_exit_before_reactivation() {
//         // 占满进程名额, 让测试只控制租约和停止边界, 不依赖真实编码速度.
//         let root = std::env::temp_dir().join(format!("anime-reactivate-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let processes = Processes::new(2, 16);
//         let held = processes.slots.acquire_many(2).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec.clone(), "first").await.unwrap();
//         output.wait_loaded().await.unwrap();
//         output.state.lock().await.encoding = true;
//         manager.release(&output.key, "first").await.unwrap();
//         let deadline = {
//             let mut state = output.state.lock().await;
//             assert_eq!(state.stop_after, Some(1));
//             let deadline = state.stop_deadline.unwrap();
//             ActiveCache::stop(&mut state);
//             assert_eq!(state.stop_deadline, Some(deadline));
//             deadline
//         };
//         assert!(deadline > Instant::now());
//         let shared = manager.acquire(spec.clone(), "second").await.unwrap();
//         assert!(Arc::ptr_eq(&output, &shared));
//         {
//             let mut state = output.state.lock().await;
//             assert_eq!(state.clients.len(), 1);
//             assert!(state.stop_after.is_none() && state.stop_deadline.is_none());
//             assert!(!*state.cancel.borrow());
//             state.encoding = false;
//             state.finalizing = true;
//         }
//         let waiting = manager.acquire(spec, "third");
//         tokio::pin!(waiting);
//         assert!(tokio::time::timeout(Duration::from_millis(30), &mut waiting).await.is_err());
//         manager.release(&output.key, "second").await.unwrap();
//         let next = tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap().unwrap();
//         assert!(!Arc::ptr_eq(&output, &next));
//         assert_eq!(manager.inner.entries.lock().await.len(), 1);
//         manager.release(&next.key, "third").await.unwrap();
//         manager.shutdown().await.unwrap();
//         drop(held);
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }
//
//     #[tokio::test]
//     async fn activation_io_failure_does_not_leave_an_unleased_active_entry() {
//         // 用普通文件阻断目录创建, 同时触发恢复失败和最后释放时的 mtime 写入失败.
//         let root = std::env::temp_dir().join(format!("anime-activation-failure-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), Processes::new(2, 16)).await.unwrap();
//         let playback = root.join("playback");
//         tokio::fs::remove_dir(&playback).await.unwrap();
//         tokio::fs::write(&playback, b"not a directory").await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec, "failed").await.unwrap();
//         assert!(output.wait_loaded().await.is_err());
//         assert!(manager.release(&output.key, "failed").await.is_err());
//         assert!(manager.get(&output.key).await.is_err());
//         tokio::fs::remove_file(&playback).await.unwrap();
//         tokio::fs::create_dir(&playback).await.unwrap();
//         manager.shutdown().await.unwrap();
//         tokio::fs::remove_dir_all(root).await.unwrap();
//     }
//
//     #[tokio::test]
//     async fn publication_monitor_never_waits_for_the_future_it_is_polling() {
//         // 模拟提交持锁等待磁盘, 防止监控分支与同一 select 中的提交 future 自锁.
//         let root = std::env::temp_dir().join(format!("anime-publication-{}", random_id().unwrap()));
//         // 测试负责创建缓存挂载目录, 管理器要求根目录已经存在.
//         tokio::fs::create_dir(&root).await.unwrap();
//         let processes = Processes::new(2, 16);
//         let held = processes.slots.acquire_many(2).await.unwrap();
//         let manager = CacheManager::new(root.clone(), PlaybackConfig::default(), processes.clone()).await.unwrap();
//         let (info, request) = ffmpeg::fixture();
//         let spec = EncodeSpec::new(&"a".repeat(32), "video.mkv".into(), info, request, None).unwrap();
//         let output = manager.acquire(spec, "test").await.unwrap();
//         let cancel = output.state.lock().await.cancel.subscribe();
//         let mut child = tokio::process::Command::new("sleep").arg("5").kill_on_drop(true).spawn().unwrap();
//         let step = async {
//             let _state = output.state.lock().await;
//             tokio::time::sleep(Duration::from_millis(150)).await;
//             Ok(42)
//         };
//         let result = tokio::time::timeout(Duration::from_secs(1),
//             manager.checked_step(&output, &mut child, &mut None, Instant::now(), 0, &cancel, step)).await;
//         child.kill().await.unwrap();
//         child.wait().await.unwrap();
//         manager.release(&output.key, "test").await.unwrap();
//         manager.shutdown().await.unwrap();
//         drop(held);
//         tokio::fs::remove_dir_all(root).await.unwrap();
//         assert_eq!(result.unwrap().unwrap(), 42);
//     }
// }
