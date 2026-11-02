
// backend/src/sessions.rs
//! 为 API 保存不透明 session 到缓存文件夹的唯一映射.

use std::{collections::{hash_map::Entry, HashMap}, path::PathBuf};

use anyhow::{bail, Context, Result};
use tokio::sync::Mutex;

#[cfg(test)]
#[path = "tests/api/sessions.rs"]
mod tests;

/// 只管理会话映射, 不持有缓存管理器或播放状态.
pub(crate) struct SessionManager {
    /// 会话标识到缓存实例文件夹的映射.
    sessions: Mutex<HashMap<String, PathBuf>>,
}

impl SessionManager {
    /// 创建空的会话映射表.
    pub(crate) fn new() -> Self {
        Self { sessions: Mutex::new(HashMap::new()) }
    }

    /// 为缓存文件夹生成新的不透明标识, 随机冲突时拒绝覆盖现有会话.
    pub(crate) async fn create(&self, folder: PathBuf) -> Result<String> {
        let session_id = crate::utils::random_id()?;
        match self.sessions.lock().await.entry(session_id.clone()) {
            Entry::Vacant(entry) => { entry.insert(folder); },
            Entry::Occupied(_) => bail!("session ID collision"),
        }
        Ok(session_id)
    }

    /// 查询会话对应的缓存文件夹, 不续期或改变缓存状态.
    pub(crate) async fn folder(&self, session_id: &str) -> Result<PathBuf> {
        self.sessions.lock().await.get(session_id).cloned().context("playback session not found")
    }

    /// 幂等删除会话映射, 不停止共享缓存生成.
    pub(crate) async fn release(&self, session_id: &str) {
        self.sessions.lock().await.remove(session_id);
    }

    /// 服务关闭时清空所有会话映射.
    pub(crate) async fn shutdown(&self) {
        self.sessions.lock().await.clear();
    }
}
