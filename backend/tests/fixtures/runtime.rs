
// backend/tests/fixtures/runtime.rs
//! 为独立管理器测试提供隔离目录和合成媒体夹具.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::process::Command;

use crate::{cache::CacheManager, constants::CACHE_VERSION, models::{CacheSpec, MediaMetadata, StreamInfo}};

/// 保存一次测试专属的临时目录.
pub(crate) struct TestDirectory {
    /// 本次测试创建且允许清理的目录.
    pub(crate) path: PathBuf,
}

impl TestDirectory {
    /// 创建不与其他测试或服务共享的目录.
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("anime-server-test-{}", crate::utils::random_id().unwrap()));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDirectory {
    /// 只清理本次测试创建的目录, 清理失败明确报告.
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            eprintln!("test directory cleanup failed at {}: {error}", self.path.display());
        }
    }
}

/// 构造带多个视频, 音频和字幕选择的媒体模型.
pub(crate) fn metadata() -> MediaMetadata {
    MediaMetadata {
        duration: 9.0, format: "matroska,webm".to_string(), start_time: 0.0,
        video_streams: vec![stream(0, "h264", true), stream(4, "h264", false)],
        audio_streams: vec![stream(1, "aac", true), stream(3, "aac", false)],
        subtitle_streams: vec![stream(2, "ass", true)],
        attachments: Vec::new(),
    }
}

/// 构造测试用稳定身份, 不包含任何路径或等待位置.
pub(crate) fn spec() -> CacheSpec {
    CacheSpec::new("a".repeat(32), 0, Some(1), None, CACHE_VERSION.to_string())
}

/// 使用 FFmpeg 生成短小的真实音视频, 不依赖用户媒体文件.
pub(crate) async fn create_media(path: &Path) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=24",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "9",
        "-c:v", "libx264", "-threads", "1", "-pix_fmt", "yuv420p", "-c:a", "aac"]).arg(path);
    crate::process::checked_output(&mut command, 30).await?;
    Ok(())
}

/// 等待指定缓存阶段, 在出现意外失败或超时时终止测试.
pub(crate) async fn wait_phase(manager: &CacheManager, handle_id: &str, expected: &str) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let status = manager.status(handle_id).await?.status;
            if status.phase == expected { return Ok(()); }
            anyhow::ensure!(status.phase != "failed", "cache failed while waiting for {expected:?}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await?
}

/// 构造单条合法媒体流.
fn stream(stream_index: i32, codec_name: &str, is_default: bool) -> StreamInfo {
    StreamInfo {
        stream_index, codec_name: codec_name.to_string(), language: None, title: None,
        is_default, is_forced: false, color_transfer: None,
    }
}
