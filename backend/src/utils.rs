
// backend/src/utils.rs
//! 工具函数和辅助模块.

use anyhow::{ensure, Result};
use tokio::process::Command;

use crate::video::process::checked_output;
use crate::constants::FALLBACK_FONT;


// 确认 http 服务存活
pub fn http_check() -> Result<()> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:8080".parse()?, std::time::Duration::from_secs(2))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut status = [0; 12];
    stream.read_exact(&mut status)?;
    ensure!(&status == b"HTTP/1.1 200", "HTTP health check failed");
    Ok(())
}

/// 启动时验证固定输出的全部依赖, 缺少编码器, 滤镜或字体就拒绝启动, 不降级输出.
pub async fn check_tools() -> Result<()> {

    checked_output(Command::new("ffprobe").arg("-version"), 10).await?;

    let encoders = String::from_utf8(checked_output(Command::new("ffmpeg").args(["-v", "error", "-encoders"]), 10).await?)?;
    for encoder in ["libx264", "aac"] {
        ensure!(encoders.contains(encoder), "FFmpeg lacks required encoder: {encoder}");
    }

    let filters = String::from_utf8(checked_output(Command::new("ffmpeg").args(["-v", "error", "-filters"]), 10).await?)?;
    for filter in ["subtitles", "overlay", "zscale", "tonemap", "aresample", "apad", "atrim", "trim", "settb", "fps"] {
        ensure!(filters.contains(filter), "FFmpeg lacks required filter: {filter}");
    }

    // 续接必须能丢弃 AAC 预热包, 缺少过滤器时不能退回有音频间隙的输出.
    let filters = String::from_utf8(checked_output(Command::new("ffmpeg").args(["-v", "error", "-bsfs"]), 10).await?)?;
    ensure!(filters.lines().any(|filter| filter.trim() == "noise"), "FFmpeg lacks the required noise bitstream filter");
    ensure!(std::path::Path::new(FALLBACK_FONT).is_file(), "CJK fallback font is missing");
    Ok(())
}
