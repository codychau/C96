//! F1–F3 知识库核心：JSONL 追加 + 原子重写 + 指纹去重 + 30 天 TTL。
//!
//! 两条入库路径（GUI 浏览器 / HTTP 客户端）都经过 Store::save：
//!   - URL hash 命中 -> 跳过（re-fetch 不产生 dup 记录，F 验收）
//!   - 标题 shingle 命中 -> 跳过（同标题不同 URL）
//!   - 正文 simhash 汉明距离 <= 3 -> 跳过（近重复全文）
//! TTL：open / purge 时把 stored_at_ms + ttl_days <= now 的记录原子丢弃。

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;

use crate::fetch::feed::site_of;
use crate::embed;
use crate::embed::VecDoc;
use crate::index::Index;
use crate::search::normalize::normalize_url;
use crate::store::doc::now_ms;
use crate::store::doc::KbDoc;
use crate::store::doc::SCHEMA_VERSION;
use crate::store::fingerprint::hamming;
use crate::store::fingerprint::simhash;
use crate::store::fingerprint::title_shingle;
use crate::store::fingerprint::url_key;
use crate::store::jsonl;
use crate::store::state;
use crate::store::state::FetchEntry;
use crate::store::state::StoreState;

pub const DEFAULT_TTL_DAYS: u64 = 30;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// 近重复 simhash 只对足够长的正文生效（短摘要噪声大，交给 URL/标题去重）。
const MIN_NEAR_DUP_CHARS: usize = 400;
const MAX_STATE_ENTRIES: usize = 200;

#[derive(Clone, Serialize)]
pub struct StoreStats {
    pub path: String,
    pub docs: usize,
    pub bytes: u64,
    pub ttl_days: u64,
    pub purged: usize,
    pub state_entries: usize,
    pub vec_docs: usize,
    pub embed_model: String,
    pub embed_enabled: bool,
}

#[derive(Clone, Serialize)]
pub struct KbHit {
    pub url: String,
    pub title: String,
    pub host: String,
    pub source: String,
    pub score: usize,
    pub snippet: String,
    pub stored_at_ms: u64,
}

pub struct Store {
    path: PathBuf,
    state_path: PathBuf,
    ttl_days: u64,
    docs: Vec<KbDoc>,
    seen: HashSet<String>,
    shingles: HashMap<u64, String>,
    simhashes: Vec<(u64, String)>,
    state: StoreState,
    purged: usize,
    /// 阶段 G：惰性 BM25 索引缓存（save / purge / 回拨后失效）
    idx: Option<Index>,
    /// 阶段 H：向量 sidecar 文件 + 当前内存向量（按 doc hash）
    vec_path: PathBuf,
    vecs: HashMap<String, VecDoc>,
    /// 阶段 I：磁盘指纹（kb + state 的 mtime/len），服务器据此发现外部进程写入
    file_key: u64,
}

/// 默认知识库：与 GUI 的 app_data_dir 同文件（%APPDATA%\com.c96.gui\kb.jsonl）。
pub fn default_kb_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    if !appdata.is_empty() {
        return PathBuf::from(appdata).join("com.c96.gui").join("kb.jsonl");
    }
    return PathBuf::from("cache").join("kb.jsonl");
}

/// F3 状态文件与知识库同目录同名前缀：<kb stem>.state.toml。
pub fn default_state_path(kb: &Path) -> PathBuf {
    let stem = match kb.file_stem() {
        Some(s) => s.to_string_lossy().to_string(),
        None => "kb".to_string(),
    };
    return kb.with_file_name(stem + ".state.toml");
}

/// E7 抽取模式排序：raw/空（整页 innerText）< fallback（退化为去导航的整页）< cluster/semantic/article（真正的正文块）。
/// `save_refresh` 用它判断「这次重捕是不是比旧记录更干净」。
pub fn mode_rank(mode: &str) -> u8 {
    return match mode {
        "cluster" | "semantic" | "article" => 2,
        "fallback" => 1,
        _ => 0,
    };
}

pub fn make_snippet(text: &str, q: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower = text.to_lowercase();
    let pos = lower.find(q).map(|bi| lower[..bi].chars().count());
    let start = match pos {
        Some(c) if c > 60 => c - 60,
        _ => 0,
    };
    let end = std::cmp::min(chars.len(), start + width);
    return chars[start..end].iter().collect();
}

impl Store {
    /// 打开（惰性加载 + 自动 TTL 清理 + 状态文件加载）。
    pub fn open(path: &Path, ttl_days: u64) -> Result<Store, String> {
        let ttl = if ttl_days == 0 { DEFAULT_TTL_DAYS } else { ttl_days };
        let sp = default_state_path(path);
        let vp = embed::path_for(path);
        let mut s = Store {
            path: path.to_path_buf(),
            state_path: sp.clone(),
            ttl_days: ttl,
            docs: Vec::new(),
            seen: HashSet::new(),
            shingles: HashMap::new(),
            simhashes: Vec::new(),
            state: state::load(&sp.to_string_lossy().to_string()),
            purged: 0,
            idx: None,
            vec_path: vp.clone(),
            vecs: embed::load(&vp),
            file_key: 0,
        };
        for line in jsonl::read_lines(&s.path) {
            if let Some(d) = jsonl::parse_doc(&line) {
                s.index(d);
            }
        }
        s.purged = s.purge();
        let vec_before = s.vecs.len();
        s.vecs.retain(|h, _| s.seen.contains(h));
        if s.vecs.len() != vec_before { let _ = embed::rewrite(&s.vec_path, &s.vecs); }
        s.file_key = Store::disk_key(&s.path, &s.state_path, &s.vec_path);
        return Ok(s);
    }

    /// kb + state + vector 文件的 mtime/len 指纹。
    fn disk_key(path: &Path, state_path: &Path, vec_path: &Path) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for p in [path, state_path, vec_path] {
            let (mt, len) = match fs::metadata(p) {
                Ok(m) => {
                    let mt = m.modified().ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    (mt, m.len())
                }
                Err(_) => (0, 0),
            };
            h ^= mt; h = h.wrapping_mul(0x100000001b3);
            h ^= len; h = h.wrapping_mul(0x100000001b3);
        }
        return h;
    }

    /// 阶段 I：其他进程写入后按需重载（常驻 RSS 服务器必须看到外部新增）。
    /// 返回 true = 确实重载了。开销 = 2 次 stat。
    pub fn reload_if_changed(&mut self) -> bool {
        let k = Store::disk_key(&self.path, &self.state_path, &self.vec_path);
        if k == self.file_key { return false; }
        match Store::open(&self.path, self.ttl_days) {
            Ok(fresh) => { *self = fresh; return true; }
            Err(_) => { self.file_key = k; return false; }
        }
    }

    fn index(&mut self, d: KbDoc) {
        self.seen.insert(d.hash.clone());
        if d.shingle != 0 { self.shingles.insert(d.shingle, d.hash.clone()); }
        if d.simhash != 0 { self.simhashes.push((d.simhash, d.hash.clone())); }
        self.docs.push(d);
    }

    fn rebuild(&mut self) {
        let docs = self.docs.clone();
        self.docs.clear();
        self.seen.clear();
        self.shingles.clear();
        self.simhashes.clear();
        for d in docs { self.index(d); }
    }

    /// 用给定文档集整体重写文件（过期清理 / 回拨时间用；内容由调用方决定）。
    fn rewrite_docs(&self, docs: &[KbDoc]) -> Result<(), String> {
        let mut lines: Vec<String> = Vec::new();
        for d in docs {
            match serde_json::to_string(d) {
                Ok(l) => lines.push(l),
                Err(_) => { }
            }
        }
        return jsonl::rewrite_atomic(&self.path, &lines);
    }

    /// F2 去重 + F1 追加入库。返回人类可读结果（stored: ... / skip: ...）。
    pub fn save(&mut self, url: &str, title: &str, text: &str, query: &str, source: &str, min_len: usize) -> Result<String, String> {
        return self.save_with_mode(url, title, text, query, source, min_len, "");
    }

    /// 同 save，但记录抽取模式（E7：GUI 自动入库用 mode 判断旧条目是不是整页 raw）。
    pub fn save_with_mode(&mut self, url: &str, title: &str, text: &str, query: &str, source: &str, min_len: usize, mode: &str) -> Result<String, String> {
        if text.trim().is_empty() { return Ok("skip: empty text".to_string()); }
        let chars = text.chars().count();
        if chars < min_len { return Ok(format!("skip: text_len {} < {}", chars, min_len)); }
        let norm = normalize_url(url);
        if norm.is_empty() { return Ok("skip: empty url".to_string()); }
        let host = site_of(url);
        // 忽略名单（RSS 面板「忽略」/ C96 store ignore）：这些主机不再自动收录
        if state::is_ignored(&self.state, &host) {
            return Ok(format!("skip: ignored host {} (取消忽略后才会再收录)", host));
        }
        // URL 黑名单（内置搜索引擎结果页 + 用户自加规则）：导航页/SERP 不进库，防止杂乱
        if state::is_blocked(&self.state, url) {
            return Ok(format!("skip: blocked pattern (黑名单命中，{} 不收录)", url));
        }
        let hash = url_key(url);
        if self.seen.contains(&hash) { return Ok("skip: duplicate url".to_string()); }
        let sh = title_shingle(title);
        if sh != 0 {
            if let Some(prev) = self.shingles.get(&sh) {
                return Ok(format!("skip: duplicate title shingle (same as {})", prev));
            }
        }
        let sim = simhash(text);
        if chars >= MIN_NEAR_DUP_CHARS {
            for pair in &self.simhashes {
                let d = hamming(sim, pair.0);
                if d <= 3 {
                    return Ok(format!("skip: near-duplicate content (simhash distance {} of {})", d, pair.1));
                }
            }
        }
        let doc = KbDoc {
            v: SCHEMA_VERSION,
            hash: hash.clone(),
            url: url.to_string(),
            title: title.to_string(),
            text: text.to_string(),
            query: query.to_string(),
            host: host,
            source: source.to_string(),
            stored_at_ms: now_ms(),
            text_len: chars,
            shingle: sh,
            simhash: sim,
            mode: mode.to_string(),
        };
        let line = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
        jsonl::append_line(&self.path, &line)?;
        let mut extra = String::new();
        if self.embed_enabled() {
            match self.embed_and_store(&doc) {
                Ok(()) => extra = " +vector".to_string(),
                Err(e) => {
                    println!("[c96 embed] skip {}: {}", doc.hash, e);
                    extra = format!(" [vector-skip: {}]", e);
                }
            }
        }
        let msg = format!("stored: {} ({} chars, host={}, source={}){}", hash, doc.text_len, doc.host, doc.source, extra);
        self.index(doc);
        self.idx = None;
        return Ok(msg);
    }

    /// E7：GUI 浏览器重访同一 URL 时，先按普通路径入库；若撞上 `skip: duplicate url`，
    /// 且本次抽取比旧记录干净（模式升级，或正文明显变长），就用新正文替换旧记录。
    /// 这样历史上「整页 innerText（含菜单/推荐）」的条目会被一次干净重访自动修好。
    pub fn save_refresh(&mut self, url: &str, title: &str, text: &str, query: &str, source: &str, min_len: usize, mode: &str) -> Result<String, String> {
        let msg = self.save_with_mode(url, title, text, query, source, min_len, mode)?;
        if !msg.starts_with("skip: duplicate url") { return Ok(msg); }
        let chars = text.chars().count();
        if chars < min_len { return Ok(msg); }
        let hash = url_key(url);
        let idx = match self.docs.iter().position(|d| d.hash == hash) {
            Some(i) => i,
            None => return Ok(msg),
        };
        let old = self.docs[idx].clone();
        let old_rank = mode_rank(&old.mode);
        let new_rank = mode_rank(mode);
        let longer = chars >= old.text_len + 200 && chars * 10 >= old.text_len * 12;
        // 模式升级：cluster/semantic/article 是真正文容器，字数更少也换；fallback 是整页退级，必须字数不亏太多
        let upgrade = new_rank > old_rank && (new_rank >= 2 || chars * 2 >= old.text_len);
        if !upgrade && !longer {
            return Ok(format!("{} · 保留旧记录（{}->{} 字 / 模式 {}->{}）", msg, old.text_len, chars,
                if old.mode.is_empty() { "raw" } else { old.mode.as_str() },
                if mode.is_empty() { "raw" } else { mode }));
        }
        let mut d = old.clone();
        d.title = title.to_string();
        d.text = text.to_string();
        // 刷新只该换正文：主题标注（query）与来源分组（source）是用户/订阅的元数据，别顺手清掉
        if !query.trim().is_empty() { d.query = query.to_string(); }
        if !source.trim().is_empty() && d.source.trim().is_empty() { d.source = source.to_string(); }
        d.text_len = chars;
        d.shingle = title_shingle(title);
        d.simhash = simhash(text);
        d.mode = mode.to_string();
        d.stored_at_ms = now_ms();
        self.docs[idx] = d;
        self.rewrite_docs(&self.docs)?;
        self.rebuild();
        self.idx = None;
        self.reembed_doc(&self.docs[idx].clone());
        return Ok(format!("replaced: {} ({} -> {} chars, mode {} -> {})", hash, old.text_len, chars,
            if old.mode.is_empty() { "raw" } else { old.mode.as_str() },
            if mode.is_empty() { "raw" } else { mode }));
    }

    /// TTL 演示 / 测试：入库后把时间回拨 days_back 天（需要整文件重写）。
    pub fn add_backdated(&mut self, url: &str, title: &str, text: &str, query: &str, source: &str, min_len: usize, days_back: u64) -> Result<String, String> {
        let msg = self.save(url, title, text, query, source, min_len)?;
        if !msg.starts_with("stored:") { return Ok(msg); }
        let n = self.docs.len();
        if n == 0 { return Ok(msg); }
        let back = days_back.saturating_mul(DAY_MS);
        self.docs[n - 1].stored_at_ms = now_ms().saturating_sub(back);
        let snapshot = self.docs.clone();
        self.rewrite_docs(&snapshot)?;
        self.idx = None;
        return Ok(format!("{} [backdated {}d]", msg, days_back));
    }

    /// 丢弃超过 TTL 的记录（原子重写），返回丢弃条数。
    pub fn purge(&mut self) -> usize {
        if self.docs.is_empty() { return 0; }
        let now = now_ms();
        let ttl_ms = self.ttl_days.saturating_mul(DAY_MS);
        let mut keep: Vec<KbDoc> = Vec::new();
        for d in &self.docs {
            if d.stored_at_ms.saturating_add(ttl_ms) > now { keep.push(d.clone()); }
        }
        let dropped = self.docs.len() - keep.len();
        if dropped == 0 { return 0; }
        // 必须写 keep（不是 self.docs）：否则过期行会留在文件里，每次 open 都重复 purge。
        match self.rewrite_docs(&keep) {
            Ok(()) => {
                self.docs = keep;
                self.rebuild();
                self.idx = None;
                self.filter_vectors_to_docs();
                self.purged = dropped;
                self.state.last_purge_ms = now;
                self.state.ttl_days = self.ttl_days;
                let sp = self.state_path.to_string_lossy().to_string();
                let _ = state::save(&sp, &self.state);
                return dropped;
            }
            Err(_) => { return 0; }
        }
    }

    /// 删单条内容（按 url hash）：原子重写 + 重建索引，返回是否真的删掉。
    /// 供 GUI RSS 内容管理器「删条目」用 —— 删完对应 feed 立刻少一条 item。
    pub fn remove_hash(&mut self, hash: &str) -> bool {
        if self.docs.is_empty() { return false; }
        let mut keep: Vec<KbDoc> = Vec::new();
        let mut dropped = 0;
        for d in &self.docs {
            if d.hash == hash { dropped += 1; } else { keep.push(d.clone()); }
        }
        if dropped == 0 { return false; }
        match self.rewrite_docs(&keep) {
            Ok(()) => {
                self.docs = keep;
                self.rebuild();
                self.idx = None;
                self.filter_vectors_to_docs();
                return true;
            }
            Err(_) => { return false; }
        }
    }

    /// 批量删除：keep_where 返回 true 的留下，其余整批原子重写（清空来源 / 清空主机用）。
    fn remove_where(&mut self, keep_where: impl Fn(&KbDoc) -> bool) -> usize {
        if self.docs.is_empty() { return 0; }
        let mut keep: Vec<KbDoc> = Vec::new();
        let mut dropped = 0;
        for d in &self.docs {
            if keep_where(d) { keep.push(d.clone()); } else { dropped += 1; }
        }
        if dropped == 0 { return 0; }
        match self.rewrite_docs(&keep) {
            Ok(()) => {
                self.docs = keep;
                self.rebuild();
                self.idx = None;
                self.filter_vectors_to_docs();
                return dropped;
            }
            Err(_) => { return 0; }
        }
    }

    /// 删一个来源的全部内容（source 精确匹配；空串 = 无来源那批）。
    pub fn remove_source(&mut self, source: &str) -> usize {
        let want = source.to_string();
        return self.remove_where(|d| d.source != want);
    }

    /// 删一个主机的全部内容（host 大小写不敏感，空 host -> 0）。
    pub fn remove_host(&mut self, host: &str) -> usize {
        let want = host.trim().to_lowercase();
        if want.is_empty() { return 0; }
        return self.remove_where(|d| d.host.to_lowercase() != want);
    }

    /// F3：记录一次抓取状态（last_fetch / etag / seen_ids / engine）。
    pub fn record_fetch(&mut self, url: &str, engine: &str, status: u64, etag: &str, last_modified: &str) -> Result<(), String> {
        let entry = FetchEntry {
            url: url.to_string(),
            engine: engine.to_string(),
            last_fetch_ms: now_ms(),
            etag: etag.to_string(),
            last_modified: last_modified.to_string(),
            status: status,
            kb_hash: url_key(url),
        };
        state::fold(&mut self.state, entry, MAX_STATE_ENTRIES);
        self.state.ttl_days = self.ttl_days;
        let sp = self.state_path.to_string_lossy().to_string();
        if !state::save(&sp, &self.state) {
            return Err(format!("cannot write state file: {}", sp));
        }
        return Ok(());
    }

    pub fn stats(&self) -> StoreStats {
        let bytes = match fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        return StoreStats {
            path: self.path.to_string_lossy().to_string(),
            docs: self.docs.len(),
            bytes: bytes,
            ttl_days: self.ttl_days,
            purged: self.purged,
            state_entries: self.state.entries.len(),
            vec_docs: self.vec_count_for_model(),
            embed_model: self.state.embed.model.clone(),
            embed_enabled: self.embed_enabled(),
        };
    }

    pub fn state(&self) -> &StoreState {
        return &self.state;
    }

    /// 可变状态（主题任务写入用）。改完记得 persist_state()。
    pub fn state_mut(&mut self) -> &mut StoreState {
        return &mut self.state;
    }

    /// 阶段 H：当前向量配置（来自 kb.state.toml 的 [embed]）。
    pub fn embed_config(&self) -> state::EmbedConfig {
        return self.state.embed.clone();
    }

    pub fn set_embed_config(&mut self, cfg: state::EmbedConfig) -> bool {
        self.state.embed = cfg;
        return self.persist_state();
    }

    fn embed_enabled(&self) -> bool {
        let e = &self.state.embed;
        return e.enabled && !e.model.trim().is_empty() && !e.base_url.trim().is_empty();
    }

    pub fn vec_path(&self) -> String {
        return self.vec_path.to_string_lossy().to_string();
    }

    pub fn vec_count(&self) -> usize {
        return self.vecs.len();
    }

    pub fn vec_count_for_model(&self) -> usize {
        if !self.embed_enabled() { return 0; }
        let model = self.state.embed.model.trim();
        return self.vecs.values().filter(|v| v.model == model).count();
    }

    fn embed_input_for(d: &KbDoc) -> String {
        if d.title.trim().is_empty() {
            return d.text.clone();
        }
        return d.title.clone() + "\n" + &d.text;
    }

    fn persist_vectors(&mut self) -> Result<(), String> {
        return embed::rewrite(&self.vec_path, &self.vecs);
    }

    /// 尝试用当前向量模型给一条新入库文档生成向量。失败不影响正文入库。
    pub fn embed_and_store(&mut self, d: &KbDoc) -> Result<(), String> {
        if !self.embed_enabled() { return Ok(()); }
        let cfg = self.embed_config();
        let input = Store::embed_input_for(d);
        let v = embed::embed(&cfg, &input)?;
        let vd = VecDoc {
            v: embed::VEC_SCHEMA_VERSION,
            hash: d.hash.clone(),
            model: cfg.model.trim().to_string(),
            dim: v.len(),
            text_hash: d.simhash,
            stored_at_ms: now_ms(),
            vec: v,
        };
        let _ = embed::append(&self.vec_path, &vd);
        self.vecs.insert(vd.hash.clone(), vd);
        return Ok(());
    }

    fn remove_vectors(&mut self, hashes: &HashSet<String>) -> bool {
        let before = self.vecs.len();
        if hashes.is_empty() { return false; }
        self.vecs.retain(|h, _| !hashes.contains(h));
        if self.vecs.len() == before { return false; }
        let _ = self.persist_vectors();
        return true;
    }

    fn filter_vectors_to_docs(&mut self) {
        let before = self.vecs.len();
        self.vecs.retain(|h, _| self.seen.contains(h));
        if self.vecs.len() != before { let _ = self.persist_vectors(); }
    }

    /// 给 save_refresh 使用：正文变化后重算当前文档向量。
    fn reembed_doc(&mut self, d: &KbDoc) {
        if !self.embed_enabled() { return; }
        let mut one: HashSet<String> = HashSet::new();
        one.insert(d.hash.clone());
        self.remove_vectors(&one);
        if let Err(e) = self.embed_and_store(d) {
            println!("[c96 embed] re-embed skip {}: {}", d.hash, e);
        }
    }

    /// 给未入库向量的历史文档补向量（每次最多 limit 条，避免一次网络风暴）。
    pub fn backfill_vectors(&mut self, limit: usize) -> usize {
        if !self.embed_enabled() || limit == 0 { return 0; }
        let model = self.state.embed.model.trim().to_string();
        let candidates: Vec<KbDoc> = self.docs.iter().rev().filter(|d| {
            match self.vecs.get(&d.hash) {
                Some(v) => !(v.model == model && v.text_hash == d.simhash && v.vec.len() > 0),
                None => true,
            }
        }).take(limit).cloned().collect();
        let mut done = 0usize;
        for owned in candidates {
            if self.embed_and_store(&owned).is_ok() { done += 1; }
        }
        return done;
    }

    /// 当前模型下的向量候选（按 cosine 降序）。
    pub fn vector_candidates(&mut self, query: &str, limit: usize) -> Vec<(KbDoc, f32)> {
        if !self.embed_enabled() || limit == 0 || query.trim().is_empty() { return Vec::new(); }
        let cfg = self.embed_config();
        let model = cfg.model.trim().to_string();
        if self.vecs.values().filter(|v| v.model == model).count() == 0 { return Vec::new(); }
        let qv = match embed::embed(&cfg, query) {
            Ok(v) => v,
            Err(e) => {
                println!("[c96 embed] query skip: {}", e);
                return Vec::new();
            }
        };
        let mut candidates: Vec<(String, f32)> = Vec::new();
        for (hash, v) in self.vecs.iter() {
            if v.model != model || v.vec.is_empty() { continue; }
            let s = embed::cosine(&qv, &v.vec);
            if s <= 0.0 { continue; }
            candidates.push((hash.clone(), s));
        }
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(limit);
        let mut out: Vec<(KbDoc, f32)> = Vec::new();
        for (hash, score) in candidates {
            if let Some(d) = self.docs.iter().find(|x| x.hash == hash) {
                out.push((d.clone(), score));
            }
        }
        return out;
    }

    /// 阶段 G：惰性构建 / 复用 BM25 索引（save / purge / 回拨会置空重建）。
    pub fn search_index(&mut self) -> &Index {
        if self.idx.is_none() {
            let docs = self.list(0, "");
            self.idx = Some(Index::build(&docs));
        }
        return self.idx.as_ref().unwrap();
    }

    pub fn persist_state(&mut self) -> bool {
        self.state.ttl_days = self.ttl_days;
        let sp = self.state_path.to_string_lossy().to_string();
        return state::save(&sp, &self.state);
    }

    pub fn state_path(&self) -> String {
        return self.state_path.to_string_lossy().to_string();
    }

    /// 最新在前；source 非空时过滤来源。
    pub fn list(&self, limit: usize, source: &str) -> Vec<KbDoc> {
        let mut out: Vec<KbDoc> = Vec::new();
        let mut i = self.docs.len();
        while i > 0 {
            i -= 1;
            let d = &self.docs[i];
            if !source.is_empty() && d.source != source { continue; }
            out.push(d.clone());
            if limit > 0 && out.len() >= limit { break; }
        }
        return out;
    }

    /// 关键词检索（子串匹配，命中次数计分）。
    pub fn search(&self, query: &str, limit: usize) -> Vec<KbHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() { return Vec::new(); }
        let mut hits: Vec<KbHit> = Vec::new();
        for d in &self.docs {
            let hay = d.title.to_lowercase() + " " + &d.text.to_lowercase();
            let score = hay.matches(&q).count();
            if score == 0 { continue; }
            hits.push(KbHit {
                url: d.url.clone(),
                title: d.title.clone(),
                host: d.host.clone(),
                source: d.source.clone(),
                score: score,
                snippet: make_snippet(&d.text, &q, 160),
                stored_at_ms: d.stored_at_ms,
            });
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits.truncate(limit);
        return hits;
    }

    pub fn last(&self) -> Option<KbDoc> {
        return self.docs.last().cloned();
    }

    pub fn purged_count(&self) -> usize {
        return self.purged;
    }
}

#[cfg(test)]
mod tests {
    use super::default_state_path;
    use super::state;
    use super::Store;
    use super::DEFAULT_TTL_DAYS;
    use std::path::PathBuf;

    fn kb(name: &str) -> PathBuf {
        let p = PathBuf::from("_scratch").join(name);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(default_state_path(&p));
        return p;
    }

    fn long_text(tag: &str) -> String {
        let mut s = String::new();
        while s.chars().count() < 600 {
            s = s + tag + " Rust 系统编程语言强调安全与并发 Cargo 构建依赖解析编译测试 ";
        }
        return s;
    }

    #[test]
    fn open_missing_is_empty_and_ttl_default() {
        let p = kb("f_kb_empty.jsonl");
        let s = Store::open(&p, 0).unwrap();
        let st = s.stats();
        assert_eq!(st.docs, 0);
        assert_eq!(st.ttl_days, DEFAULT_TTL_DAYS);
        assert_eq!(st.purged, 0);
    }

    #[test]
    fn refetch_does_not_add_dup_records() {
        let p = kb("f_kb_dedup.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        let first = s.save("https://example.com/a?utm_source=x", "Title A", "body one", "q", "http-text", 0).unwrap();
        assert!(first.starts_with("stored:"), "{}", first);
        let again = s.save("https://example.com/a#frag", "Title A", "body one", "q", "http-text", 0).unwrap();
        assert!(again.starts_with("skip: duplicate url"), "{}", again);
        assert_eq!(s.stats().docs, 1);
        // 重新打开（模拟 re-fetch 进程）：仍不新增
        let mut s2 = Store::open(&p, 30).unwrap();
        let third = s2.save("https://example.com/a", "Title A", "body one", "q", "http-text", 0).unwrap();
        assert!(third.starts_with("skip: duplicate url"), "{}", third);
        assert_eq!(s2.stats().docs, 1);
    }

    #[test]
    fn duplicate_title_and_near_duplicate_content_are_skipped() {
        let p = kb("f_kb_fingerprint.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        let t = long_text("alpha");
        assert!(s.save("https://one.example.com/x", "Shared Title", &t, "", "http-html", 0).unwrap().starts_with("stored:"));
        let dup_title = s.save("https://two.example.com/y", "shared title!", &long_text("beta"), "", "http-html", 0).unwrap();
        assert!(dup_title.starts_with("skip: duplicate title shingle"), "{}", dup_title);
        let near = s.save("https://three.example.com/z", "Other Title", &t, "", "http-html", 0).unwrap();
        assert!(near.starts_with("skip: near-duplicate content"), "{}", near);
        assert_eq!(s.stats().docs, 1);
    }

    #[test]
    fn ttl_drops_expired_docs_on_open() {
        let p = kb("f_kb_ttl.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        assert!(s.save("https://keep.example.com/", "Keep", "fresh body", "", "http-text", 0).unwrap().starts_with("stored:"));
        let back = s.add_backdated("https://old.example.com/", "Old Probe", "stale body", "", "http-text", 0, 31).unwrap();
        assert!(back.starts_with("stored:"), "{}", back);
        assert_eq!(s.stats().docs, 2);
        // 新进程打开 -> 自动丢弃 31 天前的记录
        let s2 = Store::open(&p, 30).unwrap();
        assert_eq!(s2.stats().docs, 1);
        assert_eq!(s2.stats().purged, 1);
        assert_eq!(s2.last().unwrap().url, "https://keep.example.com/");
        // 文件本身必须已丢掉过期行：再开一次不应再 purge（回归：写 keep 而不是 self.docs）
        let s2b = Store::open(&p, 30).unwrap();
        assert_eq!(s2b.stats().purged, 0);
        assert_eq!(s2b.stats().docs, 1);
        // 1 天前的记录保留
        let mut s3 = Store::open(&p, 30).unwrap();
        assert!(s3.add_backdated("https://recent.example.com/", "Recent", "recent body", "", "http-text", 0, 1).unwrap().starts_with("stored:"));
        let s4 = Store::open(&p, 30).unwrap();
        assert_eq!(s4.stats().docs, 2);
        assert_eq!(s4.stats().purged, 0);
    }

    #[test]
    fn search_stats_and_state_round_trip() {
        let p = kb("f_kb_search.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://news.example.com/rust", "Rust 编译器", "Cargo 构建系统与 rustc 编译器", "", "http-feed", 0).unwrap();
        s.save("https://news.example.com/go", "Go 编译器", "go build 工具链", "", "http-feed", 0).unwrap();
        let hits = s.search("编译器", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].score, 2);
        assert_eq!(hits[1].score, 1);
        assert_eq!(s.search("rustc", 5).len(), 1);
        assert_eq!(s.search("", 5).len(), 0);
        s.record_fetch("https://news.example.com/rust", "http", 200, "W/\"v1\"", "Mon, 06 Oct 2026 03:00:00 GMT").unwrap();
        let st = s.stats();
        assert_eq!(st.state_entries, 1);
        assert_eq!(st.ttl_days, 30);
        let back = Store::open(&p, 30).unwrap();
        assert_eq!(back.state().entries.len(), 1);
        assert_eq!(back.state().entries[0].etag, "W/\"v1\"");
        assert_eq!(back.state().entries[0].engine, "http");
        assert!(!back.state_path().is_empty());
    }

    #[test]
    fn list_newest_first_and_source_filter() {
        let p = kb("f_kb_list.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/1", "A1", "alpha body", "", "http-feed", 0).unwrap();
        s.save("https://b.example.com/2", "B2", "bravo body", "", "gui-browser", 0).unwrap();
        let all = s.list(10, "");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].url, "https://b.example.com/2");
        let gui = s.list(10, "gui-browser");
        assert_eq!(gui.len(), 1);
        assert_eq!(gui[0].source, "gui-browser");
        assert_eq!(s.list(1, "").len(), 1);
    }

    #[test]
    fn remove_hash_drops_one_doc_and_rewrites_file() {
        let p = kb("f_kb_remove.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/rm1", "RM1", "alpha body text", "", "http-feed", 0).unwrap();
        s.save("https://b.example.com/rm2", "RM2", "bravo body text", "", "http-feed", 0).unwrap();
        let target = s.list(10, "")[0].hash.clone();
        let keep_url = s.list(10, "")[0].url.clone();
        assert!(s.remove_hash(&target));
        assert_eq!(s.stats().docs, 1);
        // 幂等：已删的 hash 再删返回 false；未知 hash 也 false
        assert!(!s.remove_hash(&target));
        assert!(!s.remove_hash("0000000000000000"));
        // 落盘：重开只剩另一条
        let back = Store::open(&p, 30).unwrap();
        assert_eq!(back.stats().docs, 1);
        assert_ne!(back.list(10, "")[0].url, keep_url);
    }

    #[test]
    fn remove_source_and_host_drop_groups() {
        let p = kb("f_kb_remove_group.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/1", "A1", "alpha one", "", "gui-browser", 0).unwrap();
        s.save("https://b.example.com/2", "B2", "bravo two", "", "gui-browser", 0).unwrap();
        s.save("https://b.example.com/3", "B3", "bravo three", "", "http-feed", 0).unwrap();
        assert_eq!(s.stats().docs, 3);
        assert_eq!(s.remove_source("gui-browser"), 2);
        assert_eq!(s.stats().docs, 1);
        assert_eq!(s.remove_source("gui-browser"), 0);
        assert_eq!(s.remove_source(""), 0);
        // 落盘：重开只剩 http-feed 那条
        let back = Store::open(&p, 30).unwrap();
        assert_eq!(back.stats().docs, 1);
        assert_eq!(back.list(10, "")[0].host, "b.example.com");
        // 清空主机：大小写不敏感、空格容忍、未知返回 0
        let mut s3 = Store::open(&p, 30).unwrap();
        s3.save("https://c.example.com/4", "C4", "charlie four", "", "http-feed", 0).unwrap();
        assert_eq!(s3.remove_host("  B.Example.COM "), 1);
        assert_eq!(s3.stats().docs, 1);
        assert_eq!(s3.remove_host("b.example.com"), 0);
        assert_eq!(s3.remove_host(""), 0);
        assert_eq!(s3.list(10, "")[0].host, "c.example.com");
        let s4 = Store::open(&p, 30).unwrap();
        assert_eq!(s4.stats().docs, 1);
        assert_eq!(s4.list(10, "")[0].host, "c.example.com");
    }

    #[test]
    fn save_skips_ignored_host_and_can_be_undone() {
        let p = kb("f_kb_ignore.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        state::ignore_host(s.state_mut(), "cn.bing.com");
        assert!(s.persist_state());
        // 忽略名单落盘
        let reopened = Store::open(&p, 30).unwrap();
        assert_eq!(reopened.state().ignored_hosts.len(), 1);
        let msg = s.save("https://cn.bing.com/dictionary?q=x", "词典", "should not be stored at all", "", "gui-browser", 0).unwrap();
        assert!(msg.starts_with("skip: ignored host"), "{}", msg);
        assert_eq!(s.stats().docs, 0);
        // 取消忽略后立刻恢复收录
        state::unignore_host(s.state_mut(), "CN.BING.COM");
        assert!(s.persist_state());
        let ok = s.save("https://cn.bing.com/dictionary?q=x", "词典", "now allowed to store", "", "gui-browser", 0).unwrap();
        assert!(ok.starts_with("stored:"), "{}", ok);
        assert_eq!(s.stats().docs, 1);
        let back = Store::open(&p, 30).unwrap();
        assert_eq!(back.state().ignored_hosts.len(), 0);
        assert_eq!(back.stats().docs, 1);
    }

    #[test]
    fn save_skips_default_search_engine_serp() {
        let p = kb("f_kb_serp.jsonl");
        let mut s = Store::open(&p, DEFAULT_TTL_DAYS).unwrap();
        // 内置黑名单：搜索引擎结果页即便有正文也不入库（防杂乱）
        let bing = s.save("https://cn.bing.com/search?q=rust", "搜索结果", "一二三搜索结果页正文很长很长很长", "", "gui-browser", 0).unwrap();
        assert!(bing.starts_with("skip: blocked pattern"), "{}", bing);
        let news = s.save("https://www.bing.com/news/search?q=ai", "新闻搜索", "新闻搜索结果页正文很长很长", "", "gui-browser", 0).unwrap();
        assert!(news.starts_with("skip: blocked pattern"), "{}", news);
        assert_eq!(s.stats().docs, 0);
        // 同一 host 的非 SERP 文章页照常收录
        let art = s.save("https://www.bing.com/articles/1", "文章", "这是一篇正常的 Bing 文章正文。", "", "gui-browser", 0).unwrap();
        assert!(art.starts_with("stored:"), "{}", art);
        assert_eq!(s.stats().docs, 1);
        // 用户自加规则命中
        state::block_pattern(s.state_mut(), "*://news.example.com/*");
        assert!(s.persist_state());
        let blk = s.save("https://news.example.com/x", "标题", "被用户黑名单拦下", "", "gui-browser", 0).unwrap();
        assert!(blk.starts_with("skip: blocked pattern"), "{}", blk);
        // 删除规则后放行
        assert_eq!(state::unblock_pattern(s.state_mut(), "*://news.example.com/*"), true);
        let through = s.save("https://news.example.com/y", "标题2", "规则删除后放行入库正文", "", "gui-browser", 0).unwrap();
        assert!(through.starts_with("stored:"), "{}", through);
        assert_eq!(s.stats().docs, 2);
    }
    #[test]
    fn recapture_replaces_raw_doc_with_cleaner_extract() {
        let p = kb("f_kb_refresh.jsonl");
        let mut s = Store::open(&p, DEFAULT_TTL_DAYS).unwrap();
        let url = "https://recapture.example.com/a";
        // 旧记录：整页 innerText（模式 raw），又长又脏（菜单 + 推荐全在里面）
        let dirty = "新闻 体育 汽车 房产 旅游 教育 时尚 科技 登录 注册 广告 推荐 ".to_string() + &"正文内容 ".repeat(80);
        let a = s.save_with_mode(url, "标题", &dirty, "", "gui-browser", 10, "raw").unwrap();
        assert!(a.starts_with("stored:"), "{}", a);
        assert_eq!(s.stats().docs, 1);
        // 重访：抽取器给出干净正文（模式 cluster），虽然更短也必须替换
        let clean = "这是抽取后的干净正文段落。".repeat(20);
        let b = s.save_refresh(url, "标题", &clean, "查询词", "gui-browser", 10, "cluster").unwrap();
        assert!(b.starts_with("replaced:"), "{}", b);
        assert_eq!(s.stats().docs, 1, "替换不应新增记录");
        let last = s.last().unwrap();
        assert!(!last.text.contains("体育"), "菜单不该留在新正文里");
        assert_eq!(last.mode, "cluster");
        assert_eq!(last.query, "查询词");
        // 同样模式、字数没变多 -> 保持旧记录（不白写盘）
        let c = s.save_refresh(url, "标题", &clean, "查询词", "gui-browser", 10, "cluster").unwrap();
        assert!(c.starts_with("skip: duplicate url"), "{}", c);
        // 低模式（fallback 整页退级）且字数大幅缩水 -> 不能把好记录换坏，保留旧记录
        let short = "退级后很短的一段话。".repeat(3);
        let d = s.save_refresh(url, "标题", &short, "查询词", "gui-browser", 10, "fallback").unwrap();
        assert!(d.starts_with("skip: duplicate url"), "{}", d);
        assert_eq!(s.last().unwrap().mode, "cluster", "不该被 fallback 短文本覆盖");
        // 刷新换正文时保留主题标注 / 来源分组（不能因为这次 capture 的 query 为空就把订阅关联清掉）
        let longer = "刷新后的正文更长一些，用来触发纯字数替换路径。".repeat(30);
        let e = s.save_refresh(url, "标题", &longer, "", "其他来源", 10, "cluster").unwrap();
        assert!(e.starts_with("replaced:"), "{}", e);
        let kept = s.last().unwrap();
        assert_eq!(kept.query, "查询词", "主题标注不该被空 query 覆盖");
        assert_eq!(kept.source, "gui-browser", "来源分组不该被改写");
        // 落盘后再打开：替换结果持久化，去重表已重建（同标题不同 URL 仍会被标题 shingle 拦住）
        let back = Store::open(&p, DEFAULT_TTL_DAYS).unwrap();
        assert_eq!(back.stats().docs, 1);
        assert_eq!(back.last().unwrap().mode, "cluster");
    }
}
