//! 测试构建共享的缓存夹具, 使用临时目录和内存数据库且不调用 ffprobe.

use std::path::PathBuf;

use crate::{config::PlaybackConfig, process::{testing::fixture, ProcessManager}};

use super::{metadata::tests::test_entry, CacheManager};

/// 创建独占临时缓存目录和已初始化 schema 的单连接内存数据库.
pub(crate) async fn manager(config: PlaybackConfig, processes: ProcessManager) -> (CacheManager, PathBuf) {
    let root = std::env::temp_dir().join(format!("anime-cache-test-{}", crate::utils::random_id().unwrap()));
    tokio::fs::create_dir(&root).await.unwrap();
    // 内存数据库每个连接相互独立, 必须限制为单连接才能共享 schema.
    let database = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1)
        .connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../../../sql/init.sql")).execute(&database).await.unwrap();
    let manager = CacheManager::new(root.clone(), config, database, processes).await.unwrap();
    (manager, root)
}

/// 登记一个虚拟媒体路径并预置元数据缓存, 之后的 media_info 不会启动 ffprobe.
pub(crate) async fn register_media(manager: &CacheManager, file_id: &str) {
    sqlx::query("INSERT INTO file (pre16mib_md5, size) VALUES (?, 1)")
        .bind(file_id).execute(&manager.inner.database).await.unwrap();
    sqlx::query("INSERT INTO file_path (path, pre16mib_md5) VALUES ('/media/test.mkv', ?)")
        .bind(file_id).execute(&manager.inner.database).await.unwrap();
    manager.inner.metadata.lock().await.insert(file_id.to_owned(), test_entry(fixture().0));
}

impl CacheManager {
    /// 返回当前活动 Map 中的全部 variant-key, 供外部模块测试观察共享情况.
    pub(crate) async fn active_variants(&self) -> Vec<String> {
        self.inner.entries.lock().await.keys().cloned().collect()
    }
}
