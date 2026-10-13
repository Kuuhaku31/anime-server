//! 校验媒体元数据缓存过期和容量裁剪的测试.

use std::time::Duration;

use super::*;

/// 构造刚访问过的元数据条目, 供缓存测试跳过外部探测进程.
pub(in crate::cache) fn test_entry(info: MediaInfo) -> MetadataEntry {
    MetadataEntry { info, last_access: SystemTime::now() }
}

// 只构造裁剪所需的最小媒体信息.
fn media_info() -> MediaInfo {
    MediaInfo {
        duration: 1.0,
        start_time: 0.0,
        video_tracks: Vec::new(),
        audio_tracks: Vec::new(),
        subtitle_tracks: Vec::new(),
        attachments: Vec::new(),
    }
}

// 过期条目先被移除, 超出容量时按最后访问时间淘汰最旧条目.
#[test]
fn metadata_pruning_expires_old_entries_and_caps_capacity() {
    let now = SystemTime::now();
    let mut entries = HashMap::new();
    entries.insert("expired".to_string(), MetadataEntry {
        info: media_info(),
        last_access: now - constants::MEDIA_INFO_CACHE_MAX_DURATION - Duration::from_secs(1),
    });
    for index in 0..constants::MEDIA_INFO_CACHE_MAX + 1 {
        entries.insert(format!("item-{index}"), MetadataEntry {
            info: media_info(),
            last_access: now - Duration::from_secs(index as u64),
        });
    }
    prune(&mut entries, now);
    assert_eq!(entries.len(), constants::MEDIA_INFO_CACHE_MAX);
    assert!(!entries.contains_key("expired"));
    assert!(!entries.contains_key(&format!("item-{}", constants::MEDIA_INFO_CACHE_MAX)));
    assert!(entries.contains_key("item-0"));
}
