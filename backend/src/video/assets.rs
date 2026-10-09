use std::path::Path;

use anyhow::{ensure, Result};
use tokio::process::Command;

use super::{ffmpeg::EncodeSpec, process::checked_output};

pub async fn prepare(spec: &EncodeSpec, directory: &Path) -> Result<()> {
    let Some(track) = spec.subtitle.as_ref().filter(|track| !track.is_bitmap()) else { return Ok(()); };
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-v", "error", "-y", "-threads", "1", "-copyts", "-i"]);
    if let Some(external) = &track.external_path {
        command.arg(external).args(["-map", "0:0"]);
    } else {
        command.arg(&spec.source).args(["-map", &format!("0:{}", track.stream_index)]);
    }
    command.args(["-c:s", "ass", "-f", "ass"]).arg(directory.join("subtitle.ass"));
    checked_output(&mut command, 120).await?;
    ensure!(tokio::fs::metadata(directory.join("subtitle.ass")).await?.len() > 0,
        "subtitle extraction produced an empty file");
    let fonts = directory.join("fonts");
    tokio::fs::create_dir(&fonts).await?;
    let mut paths = Vec::new();
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-v", "error", "-y"]);
    for font in &spec.attachments {
        let Some(extension) = &font.font_extension else { continue; };
        let path = fonts.join(format!("font-{}.{}", font.stream_index, extension));
        command.arg(format!("-dump_attachment:t:{}", font.ordinal)).arg(&path);
        paths.push(path);
    }
    if !paths.is_empty() {
        command.arg("-i").arg(&spec.source)
            .args(["-map", &format!("0:{}", spec.video_index), "-c:v", "copy", "-t", "0", "-f", "null", "-"]);
        checked_output(&mut command, 60).await?;
        for path in paths {
            let size = tokio::fs::metadata(path).await?.len();
            ensure!(size > 0 && size <= 32 * 1024 * 1024, "font attachment must be between 1 byte and 32 MiB");
        }
    }
    Ok(())
}
