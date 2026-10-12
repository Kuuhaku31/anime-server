
// backend/src/db.rs
//! 声明数据库模型和仓储模块.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::info;
use crate::constants;

mod repository;

pub(crate) use repository::get_progress;
pub(crate) use repository::save_track_preferences;
pub(crate) use repository::get_video_paths_by_id;
pub(crate) use repository::get_a_video_path_by_id;
pub(crate) use repository::get_video_files;
pub(crate) use repository::get_video_by_id;
pub(crate) use repository::save_progress;
pub(crate) use repository::set_mock_identification;
pub(crate) use repository::get_paths_and_size;
pub(crate) use repository::upsert_file;

pub(crate) use repository::delete_paths;
pub(crate) use repository::delete_path;


/// 初始化数据库连接池, 并执行初始化 SQL 脚本, 创建必要的表和索引
pub async fn initialize_database() -> Result<SqlitePool, sqlx::Error> {
    let database_options = SqliteConnectOptions::new()
        .filename(constants::DATABASE_PATH)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(database_options)
        .await?;
    sqlx::raw_sql(include_str!("../sql/init.sql"))
        .execute(&pool)
        .await?;
    info!("database initialized at {}", constants::DATABASE_PATH);
    Ok(pool)
}
