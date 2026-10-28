//! H0 向量模型接入：兼容 OpenAI / Ollama embeddings 接口 + 本地 JSONL 向量 sidecar。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::jsonl;
use crate::store::state::EmbedConfig;

pub const VEC_SCHEMA_VERSION: u32 = 1;
const EMBED_TIMEOUT_SECS: u64 = 15;
const MAX_EMBED_CHARS: usize = 6000;

#[derive(Clone, Serialize, Deserialize)]
pub struct VecDoc {
    pub v: u32,
    pub hash: String,
    pub model: String,
    pub dim: usize,
    pub text_hash: u64,
    pub stored_at_ms: u64,
    pub vec: Vec<f32>,
}

pub fn path_for(kb_path: &Path) -> PathBuf {
    let stem = match kb_path.file_stem() {
        Some(s) => s.to_string_lossy().to_string(),
        None => "kb".to_string(),
    };
    return kb_path.with_file_name(stem + ".vec.jsonl");
}

pub fn load(path: &Path) -> HashMap<String, VecDoc> {
    let mut out: HashMap<String, VecDoc> = HashMap::new();
    for line in jsonl::read_lines(path) {
        if let Ok(v) = serde_json::from_str::<VecDoc>(&line) {
            if !v.hash.is_empty() && v.vec.len() > 0 {
                out.insert(v.hash.clone(), v);
            }
        }
    }
    return out;
}

pub fn rewrite(path: &Path, map: &HashMap<String, VecDoc>) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    for v in map.values() {
        if let Ok(l) = serde_json::to_string(v) { lines.push(l); }
    }
    lines.sort();
    return jsonl::rewrite_atomic(path, &lines);
}

pub fn append(path: &Path, v: &VecDoc) -> Result<(), String> {
    let line = serde_json::to_string(v).map_err(|e| e.to_string())?;
    return jsonl::append_line(path, &line);
}

pub fn truncate_text(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_EMBED_CHARS { return text.to_string(); }
    return text.chars().take(MAX_EMBED_CHARS).collect();
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() { return 0.0; }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for i in 0..a.len() {
        let x = a[i];
        let y = b[i];
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 { return 0.0; }
    return dot / (na.sqrt() * nb.sqrt());
}

fn endpoint_for(base: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.is_empty() { return String::new(); }
    if b.ends_with("/embeddings") { return b.to_string(); }
    if b.ends_with("/api") { return b.to_string() + "/embeddings"; }
    return b.to_string() + "/embeddings";
}

fn is_ollama_native(endpoint: &str) -> bool {
    return endpoint.contains("/api/embeddings");
}

fn embedding_from_json(v: &serde_json::Value) -> Result<Vec<f32>, String> {
    let arr: Option<&Vec<serde_json::Value>> = if let Some(data) = v.get("data").and_then(|x| x.as_array()) {
        if data.is_empty() { return Err("embedding data is empty".to_string()); }
        data[0].get("embedding").and_then(|x| x.as_array())
    } else if let Some(arr) = v.get("embedding").and_then(|x| x.as_array()) {
        Some(arr)
    } else if let Some(arr) = v.as_array() {
        Some(arr)
    } else {
        None
    };
    let arr = match arr {
        Some(a) => a,
        None => { return Err("embedding field not found".to_string()); }
    };
    let mut out: Vec<f32> = Vec::new();
    for x in arr {
        if let Some(n) = x.as_f64() { out.push(n as f32); }
    }
    if out.is_empty() { return Err("embedding vector is empty".to_string()); }
    return Ok(out);
}

fn embedding_text(v: &serde_json::Value) -> String {
    let msg = v.get("message").and_then(|x| x.as_str()).or_else(|| v.get("error").and_then(|x| x.as_str()));
    if let Some(m) = msg { return m.to_string(); }
    return "embedding endpoint error".to_string();
}

pub fn embed(cfg: &EmbedConfig, text: &str) -> Result<Vec<f32>, String> {
    if !cfg.enabled { return Err("embedding disabled".to_string()); }
    if cfg.model.trim().is_empty() { return Err("embedding model is empty".to_string()); }
    let endpoint = endpoint_for(&cfg.base_url);
    if endpoint.is_empty() { return Err("embedding base_url is empty".to_string()); }
    let text = truncate_text(text);
    let body = if is_ollama_native(&endpoint) {
        serde_json::json!({ "model": cfg.model, "prompt": text })
    } else {
        serde_json::json!({ "model": cfg.model, "input": [text] })
    };
    let body_s = body.to_string();
    let agent = ureq::builder().timeout(Duration::from_secs(EMBED_TIMEOUT_SECS)).build();
    let mut req = agent.post(&endpoint).set("Content-Type", "application/json");
    let auth;
    if !cfg.api_key.trim().is_empty() {
        auth = format!("Bearer {}", cfg.api_key.trim());
        req = req.set("Authorization", &auth);
    }
    let resp = match req.send_string(&body_s) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let txt = r.into_string().unwrap_or_default();
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&txt);
            let msg = match parsed {
                Ok(j) => embedding_text(&j),
                Err(_) => if txt.trim().is_empty() { format!("embedding HTTP {}", code) } else { txt },
            };
            return Err(msg);
        }
        Err(e) => { return Err(format!("embedding request failed: {}", e)); }
    };
    let txt = resp.into_string().map_err(|e| format!("embedding response read failed: {}", e))?;
    let j: serde_json::Value = serde_json::from_str(&txt).map_err(|e| format!("embedding response JSON failed: {}", e))?;
    let mut out = embedding_from_json(&j)?;
    if cfg.dim > 0 && out.len() != cfg.dim {
        out.truncate(cfg.dim);
        if out.len() != cfg.dim { return Err(format!("embedding dim {} != configured {}", out.len(), cfg.dim)); }
    }
    return Ok(out);
}
