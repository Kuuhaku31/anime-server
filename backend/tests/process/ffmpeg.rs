
// backend/tests/process/ffmpeg.rs
//! 验证 CacheSpec 命令构造, 精确续接和清单边界.

use super::*;

/// 路径和线程只改变执行参数, 不改变身份或续接的音视频采样原点.
#[test]
fn command_uses_selected_video_and_preserves_resume_timing() -> Result<()> {
    let processes = ProcessManager::new(2, 8);
    let mut spec = crate::testing::spec();
    spec.video_stream_index = 4;
    let metadata = crate::testing::metadata();
    let identity = spec.handle_id()?;
    let command = processes.command(&spec, Path::new("/media/source.mkv"), &metadata,
        Path::new("/tmp"), None, Tail::default(), 8)?;
    let args: Vec<_> = command.as_std().get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert!(!args.contains(&"-ss"));
    assert!(args.windows(2).any(|args| args == ["-map", "0:4"]));
    assert!(args.windows(2).any(|args| args == ["-threads", "8"]));
    let command = processes.command(&spec, Path::new("/media/copy.mkv"), &metadata,
        Path::new("/tmp"), None, Tail { video: 720_000, audio: 385_024 }, 2)?;
    let args: Vec<_> = command.as_std().get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert!(args.windows(2).any(|args| args == ["-ss", "7.000000000"]));
    assert!(args.iter().any(|arg| arg.contains("atrim=start_sample=376832")));
    assert_eq!(args.iter().filter(|arg| **arg == "-i").count(), 2);
    assert_eq!(identity, spec.handle_id()?);
    Ok(())
}

/// 清单必须是有限时长的连续本地分片, 内部文件不能作为播放资源.
#[test]
fn playlists_require_local_continuous_segments_and_finite_durations() {
    let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nsegment_000000.m4s\n#EXT-X-ENDLIST\n";
    let playlist = parse_playlist(text).unwrap();
    assert!(playlist.complete);
    assert_eq!(playlist.duration, 4.0);
    for invalid in [
        text.replace("#EXTINF:4,", "#EXTINF:NaN,"),
        text.replace("segment_000000.m4s", "../init.mp4"),
        text.replace("segment_000000.m4s", "segment_000001.m4s"),
        text.replace("segment_000000.m4s\n", ""),
        format!("{text}#EXTINF:4,\nsegment_000001.m4s\n"),
    ] { assert!(parse_playlist(&invalid).is_err()); }
    for name in ["../main.db", "instance.json", "complete", ".work-abc", "segment_1.m4s", "index.m3u8.tmp"] {
        assert!(!media_filename(name));
    }
}
