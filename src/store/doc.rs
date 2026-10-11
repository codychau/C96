//! F1 文档模型：一条 RAG 记录（schema 版本 + 入库时间 + 指纹）。

use serde::Deserialize;
use serde::Serialize;

/// JSONL schema 版本（F1）；旧行缺少 v 时按 0 处理。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
pub struct KbDoc {
    #[serde(default)]
    pub v: u32,
    /// FNV-1a64(normalize_url(url)) 的 16 hex —— 去重主键。
    pub hash: String,
    pub url: String,
    #[serde(default)]
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub host: String,
    /// 来源：gui-browser / http-feed / http-html / http-text / feed-file / html-file / serp / manual。
    #[serde(default)]
    pub source: String,
    /// 入库时间（Unix ms）—— TTL 以它为准。
    #[serde(default)]
    pub stored_at_ms: u64,
    #[serde(default)]
    pub text_len: usize,
    /// F2 标题 shingle hash。
    #[serde(default)]
    pub shingle: u64,
    /// F2 正文 simhash。
    #[serde(default)]
    pub simhash: u64,
}

pub fn now_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => return d.as_millis() as u64,
        Err(_) => return 0,
    }
}
