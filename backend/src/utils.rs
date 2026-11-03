
// backend/src/utils.rs
//! 提供健康检查, 媒体工具检查, 内容标识和原子文件发布辅助函数.

use std::fs::File;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use md5::Md5;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::process::checked_output;
use crate::constants::FALLBACK_FONT;

use std::io::Read;
use md5::Digest;


/// 确认本机固定端口的 HTTP 健康接口可用.
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


/// 检查稳定身份是否为完整的十六进制 MD5.
pub(crate) fn check_md5_ok(input: &str) -> bool {
    input.len() == 32 && input.bytes().all(|byte| byte.is_ascii_hexdigit())
}


/// 读取文件指定长度前缀并计算 MD5 内容身份.
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


/// 从操作系统随机源生成不透明的 128 位标识.
pub(crate) fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// 在同目录写入并同步临时文件后原子替换目标, 调用方串行化同名发布.
pub(crate) async fn atomic_file(directory: &Path, name: &str, content: &[u8]) -> Result<()> {
    let temporary = directory.join(format!("{name}.tmp"));
    let mut file = tokio::fs::File::create(&temporary).await?;
    let result = async {
        file.write_all(content).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, directory.join(name)).await?;
        sync_directory(directory).await
    }.await;
    if result.is_err() {
        match tokio::fs::remove_file(&temporary).await {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error).context(format!("cannot clean temporary file after {result:?}")),
        }
    }
    result
}

/// 确保原子替换后的目录项已经持久化.
pub(crate) async fn sync_directory(directory: &Path) -> Result<()> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(directory)?.sync_all()).await??;
    Ok(())
}
