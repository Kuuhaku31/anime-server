
// backend/src/process/assets.rs
//! 在编码任务私有目录内准备字幕和字体资源.

use std::{path::{Path, PathBuf}, time::Instant};

use anyhow::{ensure, Context, Result};
use tokio::{process::Command, sync::watch};

use crate::models::{CacheSpec, MediaMetadata};

use super::{cancelled, checked_output_cancellable, Cancelled, ProcessManager};

/// 保存仅属于本次编码任务的字幕和字体路径.
pub(crate) struct RenderAssets {
    /// 从选中字幕流提取的 ASS 文件.
    subtitle: PathBuf,
    /// 使用安全随机文件名导出的字体目录.
    fonts: PathBuf,
}

impl RenderAssets {
    /// 在私有工作目录中分配渲染资源, 随工作目录统一清理.
    pub(crate) async fn allocate(directory: &Path) -> Result<Self> {
        let fonts = directory.join("fonts");
        tokio::fs::create_dir(&fonts).await?;
        Ok(Self { subtitle: directory.join("subtitle.ass"), fonts })
    }

    /// 返回只包含固定相对路径的字幕滤镜, 不插入媒体文件名.
    pub(super) fn filter(&self) -> &'static str {
        "subtitles=filename='subtitle.ass':fontsdir='fonts'"
    }
}

impl ProcessManager {
    /// 在进程预算内抽取选中的文本字幕和安全字体附件.
    pub(crate) async fn prepare_assets(
        &self, assets: &RenderAssets, spec: &CacheSpec, source_path: &Path,
        metadata: &MediaMetadata, seconds: u64, cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let _permit = tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled.into()),
            permit = self.process_slots.acquire() => permit?,
        };
        let subtitle_stream_index = spec.subtitle_stream_index.context("text subtitle is missing")?;
        let started = Instant::now();
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y", "-threads", "1", "-copyts", "-i"])
            .arg(source_path).args(["-map", &format!("0:{subtitle_stream_index}"), "-c:s", "ass", "-f", "ass"])
            .arg(&assets.subtitle);
        checked_output_cancellable(&mut command, seconds, cancel).await?;
        ensure!(tokio::fs::metadata(&assets.subtitle).await?.len() > 0, "subtitle extraction produced an empty file");
        let mut paths = Vec::new();
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y"]);
        for font in &metadata.attachments {
            let Some(extension) = &font.font_extension else { continue; };
            let path = assets.fonts.join(format!("{}.{}", crate::utils::random_id()?, extension));
            command.arg(format!("-dump_attachment:t:{}", font.ordinal)).arg(&path);
            paths.push(path);
        }
        if !paths.is_empty() {
            let remaining = seconds.checked_sub(started.elapsed().as_secs())
                .filter(|seconds| *seconds > 0).context("subtitle preparation timed out")?;
            command.arg("-i").arg(source_path)
                .args(["-map", &format!("0:{}", spec.video_stream_index), "-c:v", "copy", "-t", "0", "-f", "null", "-"]);
            checked_output_cancellable(&mut command, remaining, cancel).await?;
            for path in paths {
                let size = tokio::fs::metadata(path).await?.len();
                ensure!(size > 0 && size <= 32 * 1024 * 1024, "font attachment must be between 1 byte and 32 MiB");
            }
        }
        Ok(())
    }
}
