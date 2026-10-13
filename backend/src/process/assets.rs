//! 准备并清理字幕和字体渲染资源.

use std::{path::{Path, PathBuf}, time::Instant};

use anyhow::{ensure, Context, Result};
use tokio::{process::Command, sync::watch};

use super::{models::EncodeSpec, checked_output_cancellable};

/// 资源只属于本次字幕渲染任务, 不进入持久播放配置或缓存身份.
pub(crate) struct RenderAssets {
    generation_id: String,
    subtitle: PathBuf,
    fonts: PathBuf,
}

impl RenderAssets {
    pub(crate) async fn allocate(root: &Path) -> Result<Self> {
        let generation_id = crate::utils::random_id()?;
        let subtitle = root.join("subtitles").join(format!("{generation_id}.ass"));
        let fonts = root.join("fonts").join(&generation_id);
        tokio::fs::create_dir(&fonts).await?;
        if let Err(error) = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&subtitle).await {
            tokio::fs::remove_dir(&fonts).await.context("cannot clean unused font directory")?;
            return Err(error).context("cannot allocate temporary subtitle");
        }
        Ok(Self { generation_id, subtitle, fonts })
    }

    pub(super) fn filter(&self) -> String {
        // FFmpeg 在 variant 的私有工作目录运行, 路径只包含服务端生成的十六进制名称.
        format!(
            "subtitles=filename='../../../subtitles/{}.ass':fontsdir='../../../fonts/{}'",
            self.generation_id, self.generation_id,
        )
    }

    pub(crate) async fn prepare(&self, spec: &EncodeSpec, seconds: u64, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let track = spec.subtitle.as_ref().context("text subtitle is missing")?;
        let started = Instant::now();
        // 保留原始字幕时间抽取为 ASS, 续接后仍能烧录在边界之前已开始的字幕.
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y", "-threads", "1", "-copyts", "-i"])
            .arg(&spec.source).args(["-map", &format!("0:{}", track.stream_index), "-c:s", "ass", "-f", "ass"])
            .arg(&self.subtitle);
        checked_output_cancellable(&mut command, seconds, cancel).await?;
        ensure!(tokio::fs::metadata(&self.subtitle).await?.len() > 0, "subtitle extraction produced an empty file");
        let mut paths = Vec::new();
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y"]);
        for font in &spec.info.attachments {
            let Some(extension) = &font.font_extension else { continue; };
            // 输出路径由服务端随机命名, 不信任容器内附件的文件名或目录.
            let path = self.fonts.join(format!("{}.{}", crate::utils::random_id()?, extension));
            command.arg(format!("-dump_attachment:t:{}", font.ordinal)).arg(&path);
            paths.push(path);
        }
        if !paths.is_empty() {
            // 字幕提取和字体导出共用准备预算, 不能分别消耗一遍完整超时.
            let remaining = seconds.checked_sub(started.elapsed().as_secs())
                .filter(|seconds| *seconds > 0).context("subtitle preparation timed out")?;
            command.arg("-i").arg(&spec.source)
                .args(["-map", &format!("0:{}", spec.config.video_stream()), "-c:v", "copy", "-t", "0", "-f", "null", "-"]);
            checked_output_cancellable(&mut command, remaining, cancel).await?;
            for path in paths {
                let size = tokio::fs::metadata(path).await?.len();
                ensure!(size > 0 && size <= 32 * 1024 * 1024, "font attachment must be between 1 byte and 32 MiB");
            }
        }
        Ok(())
    }

    pub(crate) async fn cleanup(self) -> Result<()> {
        let (subtitle, fonts) = tokio::join!(
            tokio::fs::remove_file(self.subtitle),
            tokio::fs::remove_dir_all(self.fonts),
        );
        let errors: Vec<_> = [subtitle, fonts].into_iter().filter_map(Result::err)
            .filter(|error| error.kind() != std::io::ErrorKind::NotFound).collect();
        ensure!(errors.is_empty(), "temporary render cleanup failed: {errors:?}");
        Ok(())
    }
}

/// 只清理符合本程序随机命名规则的遗留资源, 不处理目录中的其他文件.
pub(crate) async fn clean_startup(root: &Path) -> Result<()> {
    for directory in ["subtitles", "fonts"] {
        let mut entries = tokio::fs::read_dir(root.join(directory)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let owned = if directory == "subtitles" {
                name.strip_suffix(".ass").is_some_and(|s| crate::utils::check_md5(s).is_ok())
            } else { crate::utils::check_md5(&name).is_ok() };
            if !owned { continue; }
            if entry.file_type().await?.is_dir() {
                if directory == "fonts" { tokio::fs::remove_dir_all(entry.path()).await?; }
            } else { tokio::fs::remove_file(entry.path()).await?; }
        }
    }
    Ok(())
}
