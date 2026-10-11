//! 工具函数和辅助模块.

use std::fs::File;
use std::path::Path;

use anyhow::{ensure, Result};
use md5::Md5;
use tokio::process::Command;

use crate::video::process::checked_output;
use crate::constants::FALLBACK_FONT;

use std::io::Read;
use md5::Digest;


// 确认 http 服务存活
pub(crate) fn http_check() -> Result<()> {
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
pub(crate) async fn check_tools() -> Result<()> {

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


/// 检查字符串是否符合预期的文件名 MD5 哈希格式, 自动转换为小写十六进制字符.
#[allow(dead_code)]
pub(crate) fn check_md5(input: &str) -> Result<String, String> {

    if !(input.len() == 32 && input.bytes().all(|byte| byte.is_ascii_hexdigit())) {
        return Err(format!("invalid MD5 hash: {}", input));
    }

    Ok(input.to_ascii_lowercase())
}

/// 检查字符串是否符合预期的文件名 SHA256 哈希格式, 自动转换为小写十六进制字符.
#[allow(dead_code)]
fn check_sha256(input: &str) -> Result<String, String> {

    if !(input.len() == 64 && input.bytes().all(|byte| byte.is_ascii_hexdigit())) {
        return Err(format!("invalid SHA256 hash: {}", input));
    }

    Ok(input.to_ascii_lowercase())
}


// 输入文件路径和大小限制, 计算文件从头开始指定范围的 MD5 哈希前缀.
pub(crate) fn md5_hash_prefix(path: &Path, limit: u64) -> Result<String, String> {

    let mut file = File::open(path).map_err(|e| format!("failed to open {}: {}", path.display(), e))?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut remaining = limit;

    while remaining > 0 {
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|e| e.to_string())?;
        let bytes_read = file
            .read(&mut buffer[..read_limit])
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        remaining -= bytes_read as u64;
    }

    Ok(format!("{:x}", hasher.finalize()))
}
