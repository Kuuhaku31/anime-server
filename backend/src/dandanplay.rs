
// backend/src/dandanplay.rs
//! 按开放 API 协议签名并获取识别和弹幕响应, 不持有业务管理器.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header::LOCATION, redirect::Policy, Client, Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use std::{collections::HashMap, path::Path, sync::{Arc, OnceLock, Weak}};

use serde_json::Value;
use tokio::{io::AsyncReadExt, sync::Mutex};

use crate::{constants::DDP_DANMAKU_CACHE_DIR, utils};
use crate::config::DandanplayConfig;


/// 区分本地缓存故障, 缺少凭据和上游失败, 保留正确 HTTP 语义.
#[derive(Debug)]
pub(crate) enum DanmakuError {
    /// 本地缓存读取, 校验或持久化失败.
    Cache(anyhow::Error),
    /// 上游网络, HTTP 或协议失败.
    Upstream(anyhow::Error),
}

/// 将上游压缩字段展开为可直接显示的单条弹幕.
#[derive(Debug, Serialize)]
pub(crate) struct DanmakuComment {
    /// 弹幕应出现的有限非负播放秒数.
    pub(crate) time: f64,
    /// 上游模式编号, 未知模式保留原值.
    pub(crate) mode: u32,
    /// 24 位十进制 RGB 颜色值.
    pub(crate) color: u32,
    /// 上游发送者标识, 保留原字符串.
    pub(crate) sender: String,
    /// 转为字符串的原始 cid, 缺失时为 null.
    pub(crate) comment_id: Option<String>,
    /// 原始弹幕文本, 不解析为 HTML.
    pub(crate) content: String,
}
type DanmakuResponseList = Vec<DanmakuComment>;

/// 只发送文件名, 前缀 MD5 和长度, 仅返回唯一精确匹配的剧集 ID.
pub(crate) async fn
match_file(config: &DandanplayConfig, file_name: &str, file_hash: &str, file_size: u64)
-> Result<Option<u64>>
{
    let client = https_client()?;
    let response = signed_request(&client, config, Method::POST, "/api/v2/match")?
        .json(&serde_json::json!({
            "fileName": file_name,
            "fileHash": file_hash,
            "fileSize": file_size,
            "videoDuration": 0,
            "matchMode": "hashAndFileName",
        }))
        .send().await.context("DandanPlay identification request failed")?;
    let body = response_body(response).await?;

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
            Ok(Some(matched.episode_id))
        }
        (true, _) => bail!("DandanPlay exact match must contain one episode"),
    }
}

/// 读取已有缓存或显式刷新, 返回按时间排序的规范化弹幕数组.
pub(crate) async fn
load_danmaku(config: &DandanplayConfig, episode_id: u64, refresh: bool)
-> Result<DanmakuResponseList, DanmakuError>
{
    /// 获取剧集级互斥锁, 不跨网络持有整个索引的锁.
    async fn episode_lock(episode_id: u64) -> Arc<Mutex<()>> {

        // 仅保留在途剧集的弱引用, 同一剧集网络请求和缓存发布串行执行.
        static EPISODE_LOCKS: OnceLock<Mutex<HashMap<u64, Weak<Mutex<()>>>>> = OnceLock::new();

        let mut locks = EPISODE_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&episode_id).and_then(Weak::upgrade) { return lock; }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(episode_id, Arc::downgrade(&lock));
        lock
    }
    let lock = episode_lock(episode_id).await; // 获取指定剧集的锁
    let _guard = lock.lock().await; // 持有锁直到作用域结束

    let directory = Path::new(DDP_DANMAKU_CACHE_DIR);
    let name = format!("{episode_id}.json");

    // 如果不刷新, 尝试读取缓存
    if !refresh {
        // 如果缓存存在, 直接返回解析结果
        async fn cached_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
            let mut file = tokio::fs::File::open(path).await.context("cannot open danmaku cache")?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await.context("cannot read danmaku cache")?;
            Ok(Some(bytes))
        }
        if let Some(bytes) = cached_bytes(&directory.join(&name)).await
        .map_err(DanmakuError::Cache)? {
            let comments = parse_json_bytes(&bytes)
            .context("invalid cached danmaku; use Refresh comments to replace it")
            .map_err(DanmakuError::Cache)?;
            return Ok(comments);
        }
    }

    // 获取整合后的原始弹幕 JSON, CDN 跳转不携带应用认证头且不转换简繁体.
    async fn comments(config: &DandanplayConfig, episode_id: u64) -> Result<Vec<u8>> {
        let client = https_client()?;
        let path = format!("/api/v2/comment/{episode_id}");
        let mut request = signed_request(&client, config, Method::GET, &path)?
            .query(&[("withRelated", "true"), ("chConvert", "0")]);
        tokio::time::timeout(Duration::from_secs(20), async {
            for redirects in 0..=3 {
                let response = request.send().await.map_err(reqwest::Error::without_url)
                    .context("DandanPlay comments request failed")?;
                if !response.status().is_redirection() {
                    return response_body(response).await;
                }
                ensure!(redirects < 3, "DandanPlay comments exceeded three redirects");
                let location = response.headers().get(LOCATION)
                    .context("DandanPlay redirect is missing Location")?.to_str()
                    .context("DandanPlay redirect has an invalid Location")?;
                let url = response.url().join(location).context("invalid DandanPlay redirect URL")?;
                ensure!(url.scheme() == "https" && url.host_str().is_some()
                    && url.username().is_empty() && url.password().is_none(),
                    "DandanPlay redirect must use HTTPS without URL credentials");
                // 每次重新创建未签名请求, 不将原应用认证头或 Referer 发送给 CDN.
                request = client.get(url).header("Accept", "application/json");
            }
            bail!("DandanPlay comments exceeded three redirects")
        }).await.context("DandanPlay comments request timed out")?
    }
    let bytes = comments(config, episode_id).await.map_err(DanmakuError::Upstream)?;

    // 写入缓存文件
    utils::atomic_file(directory, &name, &bytes).await
    .context("cannot persist danmaku cache").map_err(DanmakuError::Cache)?;

    let comments = parse_json_bytes(&bytes).map_err(DanmakuError::Upstream)?;
    Ok(comments)
}

// ========================================================================== //

/// 创建禁止隐式跳转且具有有界超时的 HTTPS 客户端.
fn https_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .user_agent(concat!("anime-server/", env!("CARGO_PKG_VERSION")))
        .build().context("cannot initialize DandanPlay HTTPS client")
}

/// 仅签名不含查询参数的 API 路径, 原始密钥不会进入请求.
fn signed_request(client: &Client, config: &DandanplayConfig, method: Method, path: &str) -> Result<RequestBuilder> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?.as_secs();
    let signature = STANDARD.encode(Sha256::digest(
        format!("{}{timestamp}{path}{}", config.app_id, config.app_secret).as_bytes(),
    ));
    Ok(client.request(method, format!("https://api.dandanplay.net{path}"))
        .header("Accept", "application/json")
        .header("X-AppId", &config.app_id)
        .header("X-Timestamp", timestamp.to_string())
        .header("X-Signature", signature))
}

/// 读取上游正文
async fn response_body(mut response: Response) -> Result<Vec<u8>> {
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(reqwest::Error::without_url)
        .context("cannot read DandanPlay response")? {
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<Option<String>>(&body).ok()
            .flatten().filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "upstream request was rejected".to_owned());
        bail!("DandanPlay returned HTTP {status}: {message}");
    }
    Ok(body)
}


/// 校验原始 JSON 并稳定排序, 无效条目带索引报错而不是补默认值.
fn parse_json_bytes(bytes: &[u8]) -> Result<DanmakuResponseList> {

    /// 读取上游业务状态与原始条目, count 仅用于确认合法的空弹幕响应.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawResponse {
        /// 可选的上游业务成功标志, false 必须报错.
        success: Option<bool>,
        /// 可选的业务错误码, 非零必须报错.
        error_code: Option<i64>,
        /// 上游提供的业务错误说明.
        error_message: Option<String>,
        /// 官方响应声明的条数, 仅 count=0 允许 comments 为 null.
        count: Option<u64>,
        /// 必须显式提供的弹幕列表, 逐条解析以保留失败索引.
        #[serde(deserialize_with = "Option::deserialize")]
        comments: Option<Vec<Value>>,
    }
    let raw: RawResponse = serde_json::from_slice(bytes).context("invalid DandanPlay comments response")?;

    ensure!(raw.success != Some(false) && raw.error_code.unwrap_or(0) == 0,
        "DandanPlay comments failed: {}", raw.error_message.as_deref().filter(|message| !message.trim().is_empty())
            .unwrap_or("upstream request was rejected"));
    let items = match raw.comments {
        Some(items) => items,
        None if raw.count == Some(0) => Vec::new(),
        None => bail!("DandanPlay comments must be an array, or null with count=0"),
    };

    /// 展开 p 字段并将 cid 转为精度不受 JavaScript 数字限制的文本.
    fn parse_comment(item: Value) -> Result<DanmakuComment> {

        /// 读取单条原始弹幕, 保留 cid 数字的整数精度.
        #[derive(Deserialize)]
        struct RawComment {
            /// 按逗号分隔的时间, 模式, 颜色和发送者.
            p: String,
            /// 弹幕原始文本.
            m: String,
            /// 可空的整数或字符串弹幕标识.
            cid: Option<Value>,
        }
        let raw: RawComment = serde_json::from_value(item)?;

        let parts: Vec<_> = raw.p.splitn(4, ',').collect();
        ensure!(parts.len() == 4, "p must contain time, mode, color and sender");
        let time: f64 = parts[0].parse().context("invalid comment time")?;
        ensure!(time.is_finite() && time >= 0.0, "comment time must be finite and non-negative");
        let mode = parts[1].parse::<u32>().context("invalid comment mode")?;
        let color = parts[2].parse::<u32>().context("invalid comment color")?;
        ensure!(color <= 0xFFFFFF, "comment color must be a 24-bit RGB integer");
        let comment_id = match raw.cid {
            None => None,
            Some(Value::String(id)) => Some(id),
            Some(Value::Number(id)) if id.is_i64() || id.is_u64() => Some(id.to_string()),
            Some(_) => bail!("comment cid must be an integer, string or null"),
        };
        Ok(DanmakuComment { time, mode, color, sender: parts[3].to_owned(), comment_id, content: raw.m })
    }

    let mut comments = items.into_iter().enumerate().map(|(index, item)|
        parse_comment(item).with_context(|| format!("invalid DandanPlay comment at index {index}")))
        .collect::<Result<Vec<_>>>()?;
    comments.sort_by(|left, right| left.time.total_cmp(&right.time));
    Ok(comments)
}
