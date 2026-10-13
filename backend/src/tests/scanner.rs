//! 校验媒体目录扫描识别规则的测试.

use super::*;

// 扩展名识别忽略大小写, 并拒绝无扩展名或伪装的后缀.
#[test]
fn video_extensions_are_case_insensitive() {
    // 只验证扩展名识别, 不创建或修改宿主机媒体文件.
    for extension in EXTENSIONS {
        assert!(is_video(Path::new(&format!("episode.{extension}"))));
        assert!(is_video(Path::new(&format!("episode.{}", extension.to_ascii_uppercase()))));
    }
    for path in ["episode", "episode.", "episode.txt", "episode.mkv.txt"] {
        assert!(!is_video(Path::new(path)));
    }
}
