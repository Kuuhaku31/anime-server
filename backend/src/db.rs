
// backend/src/db.rs
//! 管理唯一 SQLite 连接池并公开按媒体路径操作的仓储接口.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::models::{MediaConfigs, ScannedFile};


use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

type SqlxErr = sqlx::Error; // 为 sqlx::Error 定义别名, 便于统一管理错误类型.

type MediaPathList = Vec<(String, PathBuf, MediaConfigs)>;
type PathAndIdentityList = Vec<(PathBuf, String)>;
type OptionIdAndConfigs = Option<(String, MediaConfigs)>;
type PathConfigsList = Vec<(PathBuf, MediaConfigs)>;

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

    /// 等待全部数据库连接关闭.
    pub(crate) async fn shutdown(&self) { self.pool.close().await; }

    /// 返回所有路径及其对应的配置
    pub(crate) async fn list_paths_with_configs(&self)
    -> Result<MediaPathList, SqlxErr>
    {
        let sql = "
            SELECT fp.media_id,
                   fp.path,
                   f.position,
                   f.video_stream_index,
                   f.audio_stream_index,
                   f.subtitle_stream_index
            FROM file_path AS fp
            JOIN file AS f ON f.media_id = fp.media_id ORDER BY fp.path
        ";
        type Row = (String, String, Option<f64>, Option<i32>, Option<i32>, Option<i32>);

        let rows = sqlx::query_as::<_, Row>(sql).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(media_id, path, po, v_s, a_s, s_s)|(
            media_id,
            PathBuf::from(path),
            MediaConfigs {
                position: po,
                video_stream_index: v_s,
                audio_stream_index: a_s,
                subtitle_stream_index: s_s
            }
        )).collect())
    }

    /// 根据内容 ID 返回排序后的路径副本和共享配置.
    pub(crate) async fn paths_with_configs_by_id(&self, media_id: &str) -> Result<PathConfigsList, SqlxErr> {
        type Row = (String, Option<f64>, Option<i32>, Option<i32>, Option<i32>);
        let rows = sqlx::query_as::<_, Row>(
            "SELECT fp.path, f.position, f.video_stream_index, f.audio_stream_index, f.subtitle_stream_index \
             FROM file_path AS fp JOIN file AS f ON f.media_id = fp.media_id \
             WHERE fp.media_id = ? ORDER BY fp.path",
        ).bind(media_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(path, position, video, audio, subtitle)| (
            PathBuf::from(path),
            MediaConfigs {
                position,
                video_stream_index: video,
                audio_stream_index: audio,
                subtitle_stream_index: subtitle,
            },
        )).collect())
    }

    /// 返回所有路径
    pub(crate) async fn list_paths(&self) -> Result<PathAndIdentityList, SqlxErr> {
       type Row = (String, String);
       let sql = "SELECT path, media_id FROM file_path ORDER BY path";
       let rows = sqlx::query_as::<_, Row>(sql).fetch_all(&self.pool).await?;
       Ok(rows.into_iter().map(|(path, media_id)| (PathBuf::from(path), media_id)).collect())
    }

    /// 按稳定路径顺序查找同内容副本, 由调用方跳过已经消失的源文件.
    pub(crate) async fn paths_by_media_id(&self, media_id: &str) -> Result<Vec<PathBuf>, SqlxErr> {
        let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM file_path WHERE media_id = ? ORDER BY path")
            .bind(media_id).fetch_all(&self.pool).await?;
        Ok(paths.into_iter().map(PathBuf::from).collect())
    }

    /// 根据路径获取身份和配置
    pub(crate) async fn get_file_by_path(&self, path: &Path)
    -> Result<OptionIdAndConfigs, SqlxErr>
    {
        let path = path.to_str().ok_or_else(|| sqlx::Error::Protocol(format!("non-UTF-8 media path: {}", path.display())))?;

        let sql = "
            SELECT f.media_id,
                   f.position,
                   f.video_stream_index,
                   f.audio_stream_index,
                   f.subtitle_stream_index
            FROM file AS f
            JOIN file_path AS fp ON fp.media_id = f.media_id
            WHERE fp.path = ?
        ";
        type Row = (String, Option<f64>, Option<i32>, Option<i32>, Option<i32>);
        let rows = sqlx::query_as::<_, Row>(sql).bind(path).fetch_optional(&self.pool).await?;
        Ok(rows.map(|(id, pos, v_s, a_s, s_s)| (
            id,
            MediaConfigs {
                position: pos,
                video_stream_index: v_s,
                audio_stream_index: a_s,
                subtitle_stream_index: s_s
            }
        )))
    }

    /// 设置指定路径的播放配置
    pub(crate) async fn
    save_configs_by_path(&self, path: &Path, configs: &MediaConfigs)
    -> Result<(), SqlxErr>
    {
        let path = path.to_str().ok_or_else(|| SqlxErr::Protocol(format!("non-UTF-8 media path: {}", path.display())))?;

        let sql = "
            UPDATE file SET
                position = ?,
                video_stream_index = ?,
                audio_stream_index = ?,
                subtitle_stream_index = ?
            WHERE media_id = (SELECT media_id FROM file_path WHERE path = ?)
        ";
        let result = sqlx::query(sql)
        .bind(configs.position)
        .bind(configs.video_stream_index)
        .bind(configs.audio_stream_index)
        .bind(configs.subtitle_stream_index)
        .bind(path)
        .execute(&self.pool).await?;

        if result.rows_affected() == 0 { Err(SqlxErr::RowNotFound) }
        else { Ok(()) }
    }

    /// 在事务中替换路径的内容关联
    pub(crate) async fn replace_scanned_path(&self, file: &ScannedFile)
    -> Result<(), SqlxErr>
    {
        self.replace_scanned_paths(std::slice::from_ref(file), &[]).await
    }

    /// 删除已失效的路径
    pub(crate) async fn delete_scanned_paths(&self, paths: &[PathBuf])
    -> Result<(), SqlxErr>
    {
        self.replace_scanned_paths(&[], paths).await
    }

    /// 原子应用全量扫描结果
    pub(crate) async fn
    replace_scanned_paths(&self, new_files: &[ScannedFile], removed: &[PathBuf])
    -> Result<(), SqlxErr>
    {
        let mut transaction = self.pool.begin().await?;

        // 删除已失效的路径
        for path in removed {
            let path = path.to_str().ok_or_else(|| SqlxErr::Protocol(format!("non-UTF-8 media path: {}", path.display())))?;

            sqlx::query("DELETE FROM file_path WHERE path = ?")
            .bind(path).execute(&mut *transaction).await?;
        }

        for file in new_files {
            let path = file.path.to_str().ok_or_else(|| SqlxErr::Protocol(format!("non-UTF-8 media path: {}", file.path.display())))?;

            // 删除原有路径关联
            sqlx::query("DELETE FROM file_path WHERE path = ?")
            .bind(path).execute(&mut *transaction).await?;

            // 插入文件表
            sqlx::query("INSERT INTO file (media_id) VALUES (?) ON CONFLICT (media_id) DO NOTHING")
            .bind(&file.media_id).execute(&mut *transaction).await?;

            // 插入路径关联
            sqlx::query("INSERT INTO file_path (media_id, path) VALUES (?, ?)")
            .bind(&file.media_id).bind(path).execute(&mut *transaction).await?;
        }

        // 提交事务
        transaction.commit().await
    }
}
