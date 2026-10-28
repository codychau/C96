//! F3 状态文件：last_fetch / etag / seen_ids / engine（TOML，原子重写）。
//!
//! 与 C1 的 HTTP body 缓存（cache/http_cache.toml）互补：这里只记抓取状态，
//! 用于条件 GET 复用（etag / last_modified）、抓取节奏（last_fetch_ms）、
//! 来源标注（engine）与去重（seen_ids = 知识库 url hash 列表）。

use serde::Serialize;

use crate::config::toml::parse;
use crate::config::toml::render_block;
use crate::config::toml::Value;
use crate::search::normalize::is_blacklisted;

#[derive(Clone)]
pub struct FetchEntry {
    pub url: String,
    pub engine: String,
    pub last_fetch_ms: u64,
    pub etag: String,
    pub last_modified: String,
    pub status: u64,
    pub kb_hash: String,
}

/// 主题任务（阶段 I）：一个关键词 = 一条常驻任务。
///
/// 语义（用户原则）：没有搜过 -> 立刻搜索入库并建任务；搜过 -> 直接读 RAG；
/// RAG 数据太旧 -> 后台再搜一次；任务按 interval_secs 到期轮询。
#[derive(Clone, Serialize)]
pub struct Topic {
    pub query: String,
    /// 去重键：trim + 折叠空白 + 小写
    pub key: String,
    pub engine: String,
    pub site: String,
    pub max: usize,
    /// 每次刷新抓取正文的页数（其余候选只做相关性排序，不落库）
    pub fetch_n: usize,
    /// 到期间隔（秒）：上次运行 + interval = next_due
    pub interval_secs: u64,
    /// 新鲜期（秒）：距今小于它 -> 直接命中 RAG，不联网
    pub fresh_secs: u64,
    pub created_ms: u64,
    pub last_run_ms: u64,
    pub next_due_ms: u64,
    pub runs: u64,
    /// 累计入库文档数
    pub stored: u64,
    pub paused: bool,
    pub last_status: String,
    pub last_error: String,
    /// 最近一次刷新入库的 url hash
    pub docs: Vec<String>,
}

/// 向量模型配置（阶段 H）：配置后，RAG 入库会调用兼容 embeddings 接口生成向量并写入 sidecar。
#[derive(Clone)]
pub struct EmbedConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub dim: usize,
}

#[derive(Clone)]
pub struct StoreState {
    pub version: u64,
    pub ttl_days: u64,
    pub last_purge_ms: u64,
    pub entries: Vec<FetchEntry>,
    pub topics: Vec<Topic>,
    /// 忽略名单（小写 host）：这些主机的页面不再自动收录（RSS 面板「忽略」）。
    pub ignored_hosts: Vec<String>,
    /// URL 黑名单 glob（含 '/' 或 '://' 时匹配整条 URL，否则匹配 host）。
    /// 内置搜索引擎结果页规则 + 用户自加规则，命中则自动收录跳过（防杂乱）。
    pub blocked_patterns: Vec<String>,
    /// 浏览网页时自动收录（手动“收录”不受影响）。
    pub auto_capture: bool,
    /// 向量模型配置（RAG embedding 存储 / 检索）。
    pub embed: EmbedConfig,
}

/// 最多保留的主题任务数（超出丢弃 next_due 最早的）。
pub const MAX_TOPICS: usize = 100;

pub fn default_topic() -> Topic {
    return Topic {
        query: String::new(),
        key: String::new(),
        engine: "auto".to_string(),
        site: String::new(),
        max: 20,
        fetch_n: 5,
        interval_secs: 10800,
        fresh_secs: 21600,
        created_ms: 0,
        last_run_ms: 0,
        next_due_ms: 0,
        runs: 0,
        stored: 0,
        paused: false,
        last_status: String::new(),
        last_error: String::new(),
        docs: Vec::new(),
    };
}

pub fn default_embed() -> EmbedConfig {
    return EmbedConfig {
        enabled: false,
        base_url: "http://127.0.0.1:11434/v1".to_string(),
        api_key: String::new(),
        model: String::new(),
        dim: 0,
    };
}

pub fn default_state() -> StoreState {
    return StoreState {
        version: 1,
        ttl_days: 30,
        last_purge_ms: 0,
        entries: Vec::new(),
        topics: Vec::new(),
        ignored_hosts: Vec::new(),
        blocked_patterns: Vec::new(),
        auto_capture: false,
        embed: default_embed(),
    };
}

/// 内置搜索引擎结果页黑名单（glob 匹配整条 URL）：导航页 / SERP 不自动收录，防止杂乱。
/// 用户在面板里加的模式与这些合并生效；要放行某站可在面板删除规则或取消忽略。
pub const DEFAULT_BLOCKED_PATTERNS: [&str; 13] = [
    "*://*.bing.com/search*",
    "*://*.bing.com/news/search*",
    "*://*.bing.com/images/search*",
    "*://*.bing.com/videos/search*",
    "*://*.baidu.com/s?*",
    "*://*.google.com/search?*",
    "*://*.duckduckgo.com/?q*",
    "*://*.sogou.com/web?*",
    "*://*.so.com/s?*",
    "*://*.yandex.com/search?*",
    "*://*.brave.com/search?*",
    "*://*.ecosia.org/search?*",
    "*://*.web.bing.com/search*",
];

pub fn default_blocked_patterns() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in DEFAULT_BLOCKED_PATTERNS { out.push(p.to_string()); }
    return out;
}

/// 命中黑名单：内置规则 + 用户自加规则，任一 glob 命中即拦下（大小写不敏感）。
pub fn is_blocked(st: &StoreState, url: &str) -> bool {
    if url.is_empty() { return false; }
    let mut pats: Vec<String> = default_blocked_patterns();
    for p in &st.blocked_patterns {
        if !p.trim().is_empty() { pats.push(p.clone()); }
    }
    return is_blacklisted(url, &pats);
}

/// 加入用户黑名单（已存在 / 空 -> false）。
pub fn block_pattern(st: &mut StoreState, pattern: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() { return false; }
    if st.blocked_patterns.iter().any(|x| x == p) { return false; }
    st.blocked_patterns.push(p.to_string());
    st.blocked_patterns.sort();
    return true;
}

/// 移出用户黑名单（只删用户自加项，内置规则不受影响），返回是否真删掉。
pub fn unblock_pattern(st: &mut StoreState, pattern: &str) -> bool {
    let p = pattern.trim();
    let before = st.blocked_patterns.len();
    st.blocked_patterns.retain(|x| x != p);
    return st.blocked_patterns.len() != before;
}

/// 主题去重键（大小写/空白不敏感）。
pub fn topic_key(q: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in q.trim().chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() { out.push(' '); }
        space = false;
        for l in c.to_lowercase() { out.push(l); }
    }
    return out;
}

/// 找主题下标（按 key）。
pub fn find_topic(st: &StoreState, q: &str) -> Option<usize> {
    let k = topic_key(q);
    if k.is_empty() { return None; }
    for (i, t) in st.topics.iter().enumerate() {
        if t.key == k { return Some(i); }
    }
    return None;
}

/// 同 key 替换（并移到尾部）；max>0 时按 next_due 丢弃最旧的。
pub fn fold_topic(st: &mut StoreState, t: Topic) -> bool {
    let key = t.key.clone();
    let existed = st.topics.iter().any(|x| x.key == key);
    let mut kept: Vec<Topic> = Vec::new();
    for x in st.topics.iter() {
        if x.key != key { kept.push(x.clone()); }
    }
    kept.push(t);
    if kept.len() > MAX_TOPICS {
        kept.sort_by(|a, b| a.next_due_ms.cmp(&b.next_due_ms));
        let drop_n = kept.len() - MAX_TOPICS;
        kept.drain(0..drop_n);
    }
    st.topics = kept;
    return existed;
}

pub fn remove_topic(st: &mut StoreState, q: &str) -> bool {
    let before = st.topics.len();
    let k = topic_key(q);
    st.topics.retain(|t| t.key != k);
    return st.topics.len() != before;
}

/// 到期判定（暂停的不算）。
pub fn is_due(t: &Topic, now: u64) -> bool {
    if t.paused { return false; }
    return t.next_due_ms <= now;
}

fn str_of(v: &Value) -> String {
    match v {
        Value::Str(s) => { return s.to_string(); }
        _ => { return "".to_string(); }
    }
}

fn int_of(v: &Value) -> u64 {
    match v {
        Value::Int(i) => { if *i < 0 { return 0; } return *i as u64; }
        _ => { return 0; }
    }
}

/// 读状态文件；缺失 / 损坏 -> 默认值（永不 fatal）。
pub fn load(path: &str) -> StoreState {
    let mut st = default_state();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => { return st; }
    };
    let doc = match parse(&text) {
        Ok(d) => d,
        Err(_) => { return st; }
    };
    for t in doc.tables {
        if t.name == "meta" && !t.array {
            for pair in t.keys {
                let (k, v) = pair;
                if k == "version" { st.version = int_of(&v); }
                if k == "ttl_days" { st.ttl_days = int_of(&v); }
                if k == "last_purge_ms" { st.last_purge_ms = int_of(&v); }
                if k == "auto_capture" { st.auto_capture = int_of(&v) != 0; }
                if k == "ignored_hosts" {
                    if let Value::Arr(items) = &v {
                        for it in items {
                            if let Value::Str(s) = it {
                                let h = s.trim().to_lowercase();
                                if !h.is_empty() { st.ignored_hosts.push(h); }
                            }
                        }
                    }
                }
                if k == "blocked_patterns" {
                    if let Value::Arr(items) = &v {
                        for it in items {
                            if let Value::Str(s) = it {
                                let p = s.trim();
                                if !p.is_empty() { st.blocked_patterns.push(p.to_string()); }
                            }
                        }
                    }
                }
            }
            continue;
        }
        if t.name == "embed" && !t.array {
            for pair in t.keys {
                let (k, v) = pair;
                if k == "enabled" { st.embed.enabled = int_of(&v) != 0; }
                if k == "base_url" { st.embed.base_url = str_of(&v); }
                if k == "api_key" { st.embed.api_key = str_of(&v); }
                if k == "model" { st.embed.model = str_of(&v); }
                if k == "dim" { st.embed.dim = int_of(&v) as usize; }
            }
            continue;
        }
        if t.name == "fetch" && t.array {
            let mut e = FetchEntry {
                url: "".to_string(),
                engine: "".to_string(),
                last_fetch_ms: 0,
                etag: "".to_string(),
                last_modified: "".to_string(),
                status: 0,
                kb_hash: "".to_string(),
            };
            for pair in &t.keys {
                let (k, v) = pair;
                if k == "url" { e.url = str_of(v); }
                if k == "engine" { e.engine = str_of(v); }
                if k == "last_fetch_ms" { e.last_fetch_ms = int_of(v); }
                if k == "etag" { e.etag = str_of(v); }
                if k == "last_modified" { e.last_modified = str_of(v); }
                if k == "status" { e.status = int_of(v); }
                if k == "kb_hash" { e.kb_hash = str_of(v); }
            }
            if !e.url.is_empty() { st.entries.push(e); }
        }
        if t.name == "topic" && t.array {
            let mut tp = default_topic();
            for pair in &t.keys {
                let (k, v) = pair;
                if k == "query" { tp.query = str_of(v); }
                if k == "key" { tp.key = str_of(v); }
                if k == "engine" { tp.engine = str_of(v); }
                if k == "site" { tp.site = str_of(v); }
                if k == "max" { tp.max = int_of(v) as usize; }
                if k == "fetch_n" { tp.fetch_n = int_of(v) as usize; }
                if k == "interval_secs" { tp.interval_secs = int_of(v); }
                if k == "fresh_secs" { tp.fresh_secs = int_of(v); }
                if k == "created_ms" { tp.created_ms = int_of(v); }
                if k == "last_run_ms" { tp.last_run_ms = int_of(v); }
                if k == "next_due_ms" { tp.next_due_ms = int_of(v); }
                if k == "runs" { tp.runs = int_of(v); }
                if k == "stored" { tp.stored = int_of(v); }
                if k == "paused" { tp.paused = int_of(v) != 0; }
                if k == "last_status" { tp.last_status = str_of(v); }
                if k == "last_error" { tp.last_error = str_of(v); }
                if k == "docs" {
                    if let Value::Arr(items) = v {
                        let mut ds: Vec<String> = Vec::new();
                        for it in items {
                            if let Value::Str(s) = it { ds.push(s.clone()); }
                        }
                        tp.docs = ds;
                    }
                }
            }
            if !tp.key.is_empty() { st.topics.push(tp); }
        }
    }
    return st;
}

fn parent_dir(path: &str) -> String {
    let bytes = path.as_bytes();
    let n = bytes.len();
    let mut idx: usize = 0;
    let mut found = false;
    let mut i: usize = 0;
    while i < n {
        if bytes[i] == b'\\' || bytes[i] == b'/' { idx = i; found = true; }
        i += 1;
    }
    if !found { return "".to_string(); }
    return path[..idx].to_string();
}

/// 原子写回（先 tmp 再 rename）。
pub fn save(path: &str, st: &StoreState) -> bool {
    let mut text = "# C96 store state — last_fetch / etag / seen_ids / engine (fetch --store)\n\n".to_string();
    let mut meta: Vec<(String, Value)> = Vec::new();
    meta.push(("version".to_string(), Value::Int(st.version as i64)));
    meta.push(("ttl_days".to_string(), Value::Int(st.ttl_days as i64)));
    meta.push(("last_purge_ms".to_string(), Value::Int(st.last_purge_ms as i64)));
    meta.push(("auto_capture".to_string(), Value::Int(if st.auto_capture { 1 } else { 0 })));
    let mut seen: Vec<Value> = Vec::new();
    for e in &st.entries {
        if !e.kb_hash.is_empty() { seen.push(Value::Str(e.kb_hash.clone())); }
    }
    meta.push(("seen_ids".to_string(), Value::Arr(seen)));
    let mut ign: Vec<Value> = Vec::new();
    for h in &st.ignored_hosts { ign.push(Value::Str(h.clone())); }
    meta.push(("ignored_hosts".to_string(), Value::Arr(ign)));
    let mut blk: Vec<Value> = Vec::new();
    for p in &st.blocked_patterns { blk.push(Value::Str(p.clone())); }
    meta.push(("blocked_patterns".to_string(), Value::Arr(blk)));
    text = text + &render_block("meta", false, meta);
    text += "\n";
    let mut emb: Vec<(String, Value)> = Vec::new();
    emb.push(("enabled".to_string(), Value::Int(if st.embed.enabled { 1 } else { 0 })));
    emb.push(("base_url".to_string(), Value::Str(st.embed.base_url.clone())));
    emb.push(("api_key".to_string(), Value::Str(st.embed.api_key.clone())));
    emb.push(("model".to_string(), Value::Str(st.embed.model.clone())));
    emb.push(("dim".to_string(), Value::Int(st.embed.dim as i64)));
    text = text + &render_block("embed", false, emb);
    text += "\n";
    for e in &st.entries {
        let mut keys: Vec<(String, Value)> = Vec::new();
        keys.push(("url".to_string(), Value::Str(e.url.clone())));
        keys.push(("engine".to_string(), Value::Str(e.engine.clone())));
        keys.push(("last_fetch_ms".to_string(), Value::Int(e.last_fetch_ms as i64)));
        keys.push(("etag".to_string(), Value::Str(e.etag.clone())));
        keys.push(("last_modified".to_string(), Value::Str(e.last_modified.clone())));
        keys.push(("status".to_string(), Value::Int(e.status as i64)));
        keys.push(("kb_hash".to_string(), Value::Str(e.kb_hash.clone())));
        text = text + &render_block("fetch", true, keys);
        text += "\n";
    }
    for t in &st.topics {
        let mut keys: Vec<(String, Value)> = Vec::new();
        keys.push(("query".to_string(), Value::Str(t.query.clone())));
        keys.push(("key".to_string(), Value::Str(t.key.clone())));
        keys.push(("engine".to_string(), Value::Str(t.engine.clone())));
        keys.push(("site".to_string(), Value::Str(t.site.clone())));
        keys.push(("max".to_string(), Value::Int(t.max as i64)));
        keys.push(("fetch_n".to_string(), Value::Int(t.fetch_n as i64)));
        keys.push(("interval_secs".to_string(), Value::Int(t.interval_secs as i64)));
        keys.push(("fresh_secs".to_string(), Value::Int(t.fresh_secs as i64)));
        keys.push(("created_ms".to_string(), Value::Int(t.created_ms as i64)));
        keys.push(("last_run_ms".to_string(), Value::Int(t.last_run_ms as i64)));
        keys.push(("next_due_ms".to_string(), Value::Int(t.next_due_ms as i64)));
        keys.push(("runs".to_string(), Value::Int(t.runs as i64)));
        keys.push(("stored".to_string(), Value::Int(t.stored as i64)));
        keys.push(("paused".to_string(), Value::Int(if t.paused { 1 } else { 0 })));
        keys.push(("last_status".to_string(), Value::Str(t.last_status.clone())));
        keys.push(("last_error".to_string(), Value::Str(t.last_error.clone())));
        let mut docs: Vec<Value> = Vec::new();
        for d in &t.docs { docs.push(Value::Str(d.clone())); }
        keys.push(("docs".to_string(), Value::Arr(docs)));
        text = text + &render_block("topic", true, keys);
        text += "\n";
    }
    let dir = parent_dir(path);
    if !dir.is_empty() {
        if std::fs::create_dir_all(&dir).is_err() { return false; }
    }
    let tmp = path.to_string() + ".tmp";
    if std::fs::write(&tmp, text.as_bytes()).is_err() { return false; }
    return std::fs::rename(&tmp, path).is_ok();
}

/// 同 URL（kb_hash 相同）替换旧条目并追加到尾部；max>0 时只保留最新 max 条。
pub fn fold(st: &mut StoreState, entry: FetchEntry, max: usize) -> bool {
    let key = entry.kb_hash.clone();
    let existed = !key.is_empty() && st.entries.iter().any(|e| e.kb_hash == key);
    let mut kept: Vec<FetchEntry> = Vec::new();
    for e in st.entries.iter() {
        if e.kb_hash != key { kept.push(e.clone()); }
    }
    kept.push(entry);
    if max > 0 && kept.len() > max {
        let drop_n = kept.len() - max;
        kept.drain(0..drop_n);
    }
    st.entries = kept;
    return existed;
}

/// 找某 URL 的最近状态（原样比较，不 normalize）。
pub fn find(st: &StoreState, url: &str) -> Option<FetchEntry> {
    let mut out: Option<FetchEntry> = None;
    for e in &st.entries {
        if e.url == url { out = Some(e.clone()); }
    }
    return out;
}

/// 忽略名单判定（大小写不敏感）。
pub fn is_ignored(st: &StoreState, host: &str) -> bool {
    let h = host.trim().to_lowercase();
    if h.is_empty() { return false; }
    return st.ignored_hosts.iter().any(|x| x.to_lowercase() == h);
}

/// 加入忽略名单（已存在 / 空 host -> false）。
pub fn ignore_host(st: &mut StoreState, host: &str) -> bool {
    let h = host.trim().to_lowercase();
    if h.is_empty() { return false; }
    if is_ignored(st, &h) { return false; }
    st.ignored_hosts.push(h);
    st.ignored_hosts.sort();
    return true;
}

/// 移出忽略名单，返回是否真的删掉。
pub fn unignore_host(st: &mut StoreState, host: &str) -> bool {
    let h = host.trim().to_lowercase();
    let before = st.ignored_hosts.len();
    st.ignored_hosts.retain(|x| x.to_lowercase() != h);
    return st.ignored_hosts.len() != before;
}

/// F3 seen_ids：状态文件中记录的 url hash 列表。
pub fn seen_ids(st: &StoreState) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in &st.entries {
        if !e.kb_hash.is_empty() { out.push(e.kb_hash.clone()); }
    }
    return out;
}

#[cfg(test)]
mod tests {
    use super::default_state;
    use super::default_topic;
    use super::find;
    use super::find_topic;
    use super::fold_topic;
    use super::is_due;
    use super::remove_topic;
    use super::topic_key;
    use super::fold;
    use super::ignore_host;
    use super::is_ignored;
    use super::is_blocked;
    use super::block_pattern;
    use super::unblock_pattern;
    use super::load;
    use super::unignore_host;
    use super::save;
    use super::seen_ids;
    use super::FetchEntry;

    fn entry(url: &str, hash: &str, etag: &str) -> FetchEntry {
        return FetchEntry {
            url: url.to_string(),
            engine: "http".to_string(),
            last_fetch_ms: 12345,
            etag: etag.to_string(),
            last_modified: "Mon, 06 Oct 2026 03:00:00 GMT".to_string(),
            status: 200,
            kb_hash: hash.to_string(),
        };
    }

    #[test]
    fn save_load_round_trip() {
        let path = "_scratch/f_state_round.toml";
        let mut st = default_state();
        st.ttl_days = 30;
        st.last_purge_ms = 999;
        fold(&mut st, entry("https://a/", "aaaa", "W/\"1\""), 200);
        fold(&mut st, entry("https://b/", "bbbb", ""), 200);
        assert!(save(&path, &st));
        let back = load(&path);
        assert_eq!(back.version, 1);
        assert_eq!(back.ttl_days, 30);
        assert_eq!(back.last_purge_ms, 999);
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].url, "https://a/");
        assert_eq!(back.entries[0].etag, "W/\"1\"");
        assert_eq!(back.entries[1].kb_hash, "bbbb");
        assert_eq!(seen_ids(&back).len(), 2);
        let hit = find(&back, "https://b/");
        assert!(hit.is_some());
        assert_eq!(hit.as_ref().unwrap().last_modified, "Mon, 06 Oct 2026 03:00:00 GMT");
    }

    #[test]
    fn topic_round_trip_fold_remove_due() {
        let path = "_scratch/f_state_topics.toml";
        let mut st = default_state();
        let mut t = default_topic();
        t.query = "卡普空 最新游戏".to_string();
        t.key = topic_key(&t.query);
        t.interval_secs = 600;
        t.fresh_secs = 3600;
        t.last_run_ms = 111;
        t.next_due_ms = 222;
        t.stored = 3;
        t.docs = vec!["h1".to_string(), "h2".to_string()];
        assert_eq!(fold_topic(&mut st, t.clone()), false);
        assert_eq!(st.topics.len(), 1);
        assert!(save(&path, &st));
        let back = load(&path);
        assert_eq!(back.topics.len(), 1);
        assert_eq!(back.topics[0].query, "卡普空 最新游戏");
        assert_eq!(back.topics[0].interval_secs, 600);
        assert_eq!(back.topics[0].fresh_secs, 3600);
        assert_eq!(back.topics[0].next_due_ms, 222);
        assert_eq!(back.topics[0].stored, 3);
        assert_eq!(back.topics[0].docs.len(), 2);
        assert_eq!(back.topics[0].paused, false);
        // 大小写/空白不敏感命中
        assert_eq!(find_topic(&back, "  卡普空   最新游戏 ").is_some(), true);
        assert_eq!(find_topic(&back, "别的关键词"), None);
        // 同 key 替换不新增
        let mut t2 = back.topics[0].clone();
        t2.stored = 9;
        assert_eq!(fold_topic(&mut st, t2), true);
        assert_eq!(st.topics.len(), 1);
        assert_eq!(st.topics[0].stored, 9);
        // 到期判定
        let mut t3 = default_topic();
        t3.key = "k".to_string();
        t3.next_due_ms = 100;
        assert_eq!(is_due(&t3, 100), true);
        assert_eq!(is_due(&t3, 99), false);
        t3.paused = true;
        assert_eq!(is_due(&t3, 1000), false);
        // 删除
        assert_eq!(remove_topic(&mut st, "卡普空 最新游戏"), true);
        assert_eq!(st.topics.len(), 0);
        assert_eq!(remove_topic(&mut st, "不存在"), false);
    }

    #[test]
    fn ignored_hosts_round_trip_and_case_fold() {
        let path = "_scratch/f_state_ignored.toml";
        let mut st = default_state();
        assert_eq!(st.ignored_hosts.len(), 0);
        assert_eq!(is_ignored(&st, "cn.bing.com"), false);
        assert!(ignore_host(&mut st, "  CN.Bing.COM "));
        assert_eq!(ignore_host(&mut st, "cn.bing.com"), false);
        assert_eq!(ignore_host(&mut st, "   "), false);
        assert_eq!(is_ignored(&st, "cn.bing.com"), true);
        assert_eq!(is_ignored(&st, "CN.BING.COM"), true);
        assert_eq!(is_ignored(&st, "www.sohu.com"), false);
        ignore_host(&mut st, "www.sohu.com");
        assert_eq!(st.ignored_hosts, vec!["cn.bing.com".to_string(), "www.sohu.com".to_string()]);
        assert!(save(&path, &st));
        let back = load(&path);
        assert_eq!(back.ignored_hosts.len(), 2);
        assert_eq!(is_ignored(&back, "cn.bing.com"), true);
        assert_eq!(back.topics.len(), 0);
        assert_eq!(back.entries.len(), 0);
        let mut st2 = back.clone();
        assert_eq!(unignore_host(&mut st2, "CN.BING.COM"), true);
        assert_eq!(unignore_host(&mut st2, "cn.bing.com"), false);
        assert_eq!(is_ignored(&st2, "cn.bing.com"), false);
        assert_eq!(is_ignored(&st2, "www.sohu.com"), true);
    }

    #[test]
    fn blocked_patterns_defaults_and_round_trip() {
        let mut st = default_state();
        // 内置 SERP 规则开箱即用
        assert_eq!(is_blocked(&st, "https://cn.bing.com/search?q=rust"), true);
        assert_eq!(is_blocked(&st, "https://www.bing.com/news/search?q=ai"), true);
        assert_eq!(is_blocked(&st, "https://www.baidu.com/s?wd=rust"), true);
        assert_eq!(is_blocked(&st, "https://blog.example.com/post?id=7"), false);
        // 用户自加规则
        assert!(block_pattern(&mut st, "*.tracker.net"));
        assert_eq!(block_pattern(&mut st, "*.tracker.net"), false);
        assert_eq!(block_pattern(&mut st, "   "), false);
        assert_eq!(is_blocked(&st, "https://x.tracker.net/y"), true);
        assert!(save("_scratch/f_state_blocked.toml", &st));
        let back = load("_scratch/f_state_blocked.toml");
        assert_eq!(back.blocked_patterns, vec!["*.tracker.net".to_string()]);
        assert_eq!(is_blocked(&back, "https://x.tracker.net/y"), true);
        assert_eq!(is_blocked(&back, "https://cn.bing.com/search?q=1"), true);
        let mut st2 = back.clone();
        assert_eq!(unblock_pattern(&mut st2, "*.tracker.net"), true);
        assert_eq!(unblock_pattern(&mut st2, "*.tracker.net"), false);
        assert_eq!(is_blocked(&st2, "https://x.tracker.net/y"), false);
        // 内置规则不可被 unblock 删掉
        assert_eq!(unblock_pattern(&mut st2, "*://*.bing.com/search*"), false);
        assert_eq!(is_blocked(&st2, "https://cn.bing.com/search?q=2"), true);
    }

    #[test]
    fn load_missing_is_default() {
        let st = load("_scratch/does_not_exist_anywhere.toml");
        assert_eq!(st.entries.len(), 0);
        assert_eq!(st.ttl_days, 30);
    }

    #[test]
    fn fold_replaces_same_hash_and_prunes() {
        let mut st = default_state();
        assert_eq!(fold(&mut st, entry("https://a/", "h1", "e1"), 2), false);
        assert_eq!(fold(&mut st, entry("https://a/", "h1", "e2"), 2), true);
        assert_eq!(st.entries.len(), 1);
        assert_eq!(st.entries[0].etag, "e2");
        fold(&mut st, entry("https://b/", "h2", ""), 2);
        fold(&mut st, entry("https://c/", "h3", ""), 2);
        assert_eq!(st.entries.len(), 2);
        assert_eq!(st.entries[0].kb_hash, "h2");
        assert_eq!(st.entries[1].kb_hash, "h3");
    }
}
