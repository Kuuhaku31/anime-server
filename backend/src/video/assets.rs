//! 准备并清理字幕和字体渲染资源.

use std::{path::{Path, PathBuf}, time::Instant};

use anyhow::{ensure, Context, Result};
use tokio::{process::Command, sync::watch};

use super::{cache::{is_key, random_id}, ffmpeg::EncodeSpec, process::checked_output_cancellable};

pub struct RenderAssets {
    id: String,
    subtitle: PathBuf,
    fonts: PathBuf,
}

impl RenderAssets {
    pub async fn allocate(root: &Path) -> Result<Self> {
        let id = random_id()?;
        let subtitle = root.join("subtitles").join(format!("{id}.ass"));
        let fonts = root.join("fonts").join(&id);
        tokio::fs::create_dir(&fonts).await?;
        if let Err(error) = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&subtitle).await {
            tokio::fs::remove_dir(&fonts).await.context("cannot clean unused font directory")?;
            return Err(error).context("cannot allocate temporary subtitle");
        }
        Ok(Self { id, subtitle, fonts })
    }

    pub fn filter(&self) -> String {
        // These relative paths contain only server-generated hexadecimal names.
        format!("subtitles=filename='../../subtitles/{}.ass':fontsdir='../../fonts/{}'", self.id, self.id)
    }

    pub async fn prepare(&self, spec: &EncodeSpec, seconds: u64, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let track = spec.subtitle.as_ref().context("text subtitle is missing")?;
        let started = Instant::now();
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y", "-threads", "1", "-copyts", "-i"])
            .arg(&spec.source).args(["-map", &format!("0:{}", track.stream_index), "-c:s", "ass", "-f", "ass"])
            .arg(&self.subtitle);
        checked_output_cancellable(&mut command, seconds, cancel).await?;
        ensure!(tokio::fs::metadata(&self.subtitle).await?.len() > 0, "subtitle extraction produced an empty file");
        let mut paths = Vec::new();
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y"]);
        for font in &spec.attachments {
            let Some(extension) = &font.font_extension else { continue; };
            let path = self.fonts.join(format!("{}.{}", random_id()?, extension));
            command.arg(format!("-dump_attachment:t:{}", font.ordinal)).arg(&path);
            paths.push(path);
        }
        if !paths.is_empty() {
            let remaining = seconds.checked_sub(started.elapsed().as_secs())
                .filter(|seconds| *seconds > 0).context("subtitle preparation timed out")?;
            command.arg("-i").arg(&spec.source)
                .args(["-map", &format!("0:{}", spec.config.video_stream), "-c:v", "copy", "-t", "0", "-f", "null", "-"]);
            checked_output_cancellable(&mut command, remaining, cancel).await?;
            for path in paths {
                let size = tokio::fs::metadata(path).await?.len();
                ensure!(size > 0 && size <= 32 * 1024 * 1024, "font attachment must be between 1 byte and 32 MiB");
            }
        }
        Ok(())
    }

    pub async fn cleanup(self) -> Result<()> {
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

pub async fn clean_startup(root: &Path) -> Result<()> {
    for directory in ["subtitles", "fonts"] {
        let mut entries = tokio::fs::read_dir(root.join(directory)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let owned = if directory == "subtitles" {
                name.strip_suffix(".ass").is_some_and(is_key)
            } else { is_key(&name) };
            if !owned { continue; }
            if entry.file_type().await?.is_dir() {
                if directory == "fonts" { tokio::fs::remove_dir_all(entry.path()).await?; }
            } else { tokio::fs::remove_file(entry.path()).await?; }
        }
    }
    Ok(())
}
