
// backend/src/tests/api/sessions.rs
//! 验证不透明会话到共享缓存文件夹的独立映射.

use super::*;

/// 释放一个会话不能删除其他会话对同一文件夹的映射.
#[tokio::test]
async fn sessions_share_folders_but_release_independently() -> Result<()> {
    let sessions = SessionManager::new();
    let folder = PathBuf::from("/cache/playback/example");
    let first = sessions.create(folder.clone()).await?;
    let second = sessions.create(folder.clone()).await?;
    assert_ne!(first, second);
    sessions.release(&first).await;
    sessions.release(&first).await;
    assert!(sessions.folder(&first).await.is_err());
    assert_eq!(sessions.folder(&second).await?, folder);
    sessions.shutdown().await;
    assert!(sessions.folder(&second).await.is_err());
    Ok(())
}
