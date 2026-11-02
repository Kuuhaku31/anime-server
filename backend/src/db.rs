
// backend/src/db.rs
//! 管理唯一 SQLite 连接池并公开按媒体路径操作的仓储接口.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sqlx::{sqlite::{SqliteConnectOptions, SqlitePoolOptions}, SqlitePool};

use crate::models::{FileRecord, MediaConfigs, PathConfigs, PathRecord, ScannedFile};

mod repository;

#[cfg(test)]
#[path = "tests/db.rs"]
mod tests;

/// 保存数据库连接池, 不持有其他业务管理器.
pub(crate) struct DatabaseManager {
    /// 当前数据库唯一连接池.
    pool: SqlitePool,
}

impl DatabaseManager {
    /// 执行新 schema, 对旧数据库明确报错而不迁移或删除数据.
    pub(crate) async fn initialize(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path).create_if_missing(true).foreign_keys(true);
        let pool = SqlitePoolOptions::new().max_connections(5).connect_with(options).await?;
        sqlx::raw_sql(include_str!("../sql/init.sql")).execute(&pool).await?;
        sqlx::query("SELECT media_id, position, video_stream_index, audio_stream_index, subtitle_stream_index FROM file LIMIT 0")
            .execute(&pool).await
            .context("incompatible database schema; back up and remove the old main.db before restarting")?;
        Ok(Self { pool })
    }

    /// 返回媒体列表所需的路径和共享配置.
    pub(crate) async fn list_paths_with_configs(&self) -> Result<Vec<PathConfigs>, sqlx::Error> {
        repository::list_paths_with_configs(&self.pool).await
    }

    /// 返回扫描器需要比较的全部路径关联.
    pub(crate) async fn list_paths(&self) -> Result<Vec<PathRecord>, sqlx::Error> {
        repository::list_paths(&self.pool).await
    }

    /// 按路径读取当前内容身份和共享配置.
    pub(crate) async fn get_file_by_path(&self, path: &Path) -> Result<Option<FileRecord>, sqlx::Error> {
        repository::get_file_by_path(&self.pool, path).await
    }

    /// 按路径替换完整的播放配置.
    pub(crate) async fn save_configs_by_path(&self, path: &Path, configs: &MediaConfigs) -> Result<(), sqlx::Error> {
        repository::save_configs_by_path(&self.pool, path, configs).await
    }

    /// 在事务中替换路径的内容关联
    pub(crate) async fn replace_scanned_path(&self, file: &ScannedFile) -> Result<(), sqlx::Error> {
        repository::replace_scanned_paths(&self.pool, std::slice::from_ref(file), &[]).await
    }

    /// 原子应用全量扫描结果
    pub(crate) async fn replace_scanned_paths(&self, files: &[ScannedFile], removed: &[PathBuf]) -> Result<(), sqlx::Error> {
        repository::replace_scanned_paths(&self.pool, files, removed).await
    }

    /// 删除已失效的路径
    pub(crate) async fn delete_scanned_paths(&self, paths: &[PathBuf]) -> Result<(), sqlx::Error> {
        repository::replace_scanned_paths(&self.pool, &[], paths).await
    }

    /// 等待全部数据库连接关闭.
    pub(crate) async fn shutdown(&self) {
        self.pool.close().await;
    }
}
