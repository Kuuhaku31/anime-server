
// backend/src/dandanplay.rs
//! 按开放 API 协议签名并识别文件, 不读取视频内容或持有业务管理器.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{redirect::Policy, Client};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::DandanplayConfig;

/// 只发送文件名, 前缀 MD5 和长度, 仅返回唯一精确匹配的剧集 ID.
pub(crate) async fn
match_file(config: &DandanplayConfig, file_name: &str, file_hash: &str, file_size: u64)
-> Result<Option<u64>>
{
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?.as_secs();
    let signature = STANDARD.encode(Sha256::digest(
        format!("{}{timestamp}/api/v2/match{}", config.app_id, config.app_secret).as_bytes(),
    ));
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .user_agent(concat!("anime-server/", env!("CARGO_PKG_VERSION")))
        .build().context("cannot initialize DandanPlay HTTPS client")?;
    let mut response = client.post("https://api.dandanplay.net/api/v2/match")
        .header("Accept", "application/json")
        .header("X-AppId", &config.app_id)
        .header("X-Timestamp", timestamp.to_string())
        .header("X-Signature", signature)
        .json(&serde_json::json!({
            "fileName": file_name,
            "fileHash": file_hash,
            "fileSize": file_size,
            "videoDuration": 0,
            "matchMode": "hashAndFileName",
        }))
        .send().await.context("DandanPlay identification request failed")?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.context("cannot read DandanPlay response")? {
        ensure!(body.len() + chunk.len() <= 1024 * 1024, "DandanPlay response exceeds 1 MiB");
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<Option<String>>(&body).ok()
            .and_then(|error| error)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "upstream request was rejected".to_owned());
        bail!("DandanPlay returned HTTP {status}: {message}");
    }

    /// 只解析上游条目的剧集 ID, 不保留标题.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MatchItem {
        /// 上游关联的正整数剧集 ID.
        episode_id: u64,
    }

    /// 解析上游业务状态和判断唯一精确匹配所需的字段.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MatchResponse {
        /// 请求是否成功, 缺失时拒绝将响应视为未匹配.
        success: bool,
        /// 上游错误码, 0 表示没有业务错误.
        error_code: Option<i32>,
        /// 上游提供的业务错误原因.
        error_message: Option<String>,
        /// 精确关联标志, 成功响应必须提供.
        is_matched: Option<bool>,
        /// 上游允许为 null 的候选列表.
        matches: Option<Vec<MatchItem>>,
    }
    let response: MatchResponse = serde_json::from_slice(&body).context("invalid DandanPlay identification response")?;

    ensure!(response.success && response.error_code.unwrap_or(0) == 0, "DandanPlay identification failed: {}",
        response.error_message.as_deref().filter(|message| !message.trim().is_empty())
            .unwrap_or("upstream request was rejected"));
    let is_matched = response.is_matched.context("DandanPlay response is missing isMatched")?;
    let matches = response.matches.unwrap_or_default();
    match (is_matched, matches.as_slice()) {
        (false, _) => Ok(None),
        (true, [matched]) => {
            ensure!((1..=9_007_199_254_740_991).contains(&matched.episode_id),
                "DandanPlay episode ID must be a positive JavaScript-safe integer");
            Ok(Some(matched.episode_id))
        }
        (true, _) => bail!("DandanPlay exact match must contain one episode"),
    }
}
