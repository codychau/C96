//! 阶段 I 主题订阅层：关键词 = 常驻任务。
//!
//! 用户原则：
//!   1) 没搜过的关键词 -> \`handle_query\` 立刻搜索入库，并建立任务清单（state.toml 的 [[topic]]）；
//!      程序（GUI / \`research --watch\`）一直运行就按 interval 到期轮询刷新。
//!   2) 搜过的关键词 -> 直接读 RAG；若 \`now - last_run_ms > fresh_secs\`（数据太旧），
//!      先返回 RAG 里的旧数据，再后台 / 下一轮刷新一次。
//!
//! 与 store/kb 的分工：kb 管"怎么存、怎么过期"；这里管"什么时候搜、搜完拿什么回答"。

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use serde::Serialize;

use crate::fetch::feed::parse_feed;
use crate::fetch::html::extract;
use crate::fetch::http::default_opts;
use crate::fetch::http::fetch;
use crate::search::normalize;
use crate::search::query;
use crate::search::run as engines;
use crate::search::run::EngineReport;
use crate::search::run::RunOpts as NetOpts;
use crate::search::Candidate;
use crate::store::doc::now_ms;
use crate::store::kb::make_snippet;
use crate::store::kb::KbHit;
use crate::store::kb::Store;
use crate::store::state;
use crate::store::state::default_topic;
use crate::store::state::Topic;

pub const DEFAULT_INTERVAL_SECS: u64 = 10800;  // 180 min
pub const DEFAULT_FRESH_SECS: u64 = 6 * 3600;  // 6 h
pub const DEFAULT_MAX: usize = 20;
// 每轮进网页抽正文的候选数（相关性排序后取前 N）；抓不到的退化为 SERP 片段。
pub const DEFAULT_FETCH_N: usize = 10;
pub const DEFAULT_SCHED_TICK_SECS: u64 = 60;

/// 创建一个主题任务所需的参数（CLI 选项 / GUI 参数 → 这里）。
#[derive(Clone)]
pub struct TopicSpec {
    pub query: String,
    pub engine: String,
    pub site: String,
    pub max: usize,
    pub fetch_n: usize,
    pub interval_secs: u64,
    pub fresh_secs: u64,
}

impl TopicSpec {
    pub fn new(query: &str) -> TopicSpec {
        return TopicSpec {
            query: query.to_string(),
            engine: "auto".to_string(),
            site: String::new(),
            max: DEFAULT_MAX,
            fetch_n: DEFAULT_FETCH_N,
            interval_secs: DEFAULT_INTERVAL_SECS,
            fresh_secs: DEFAULT_FRESH_SECS,
        };
    }
}

#[derive(Clone)]
pub struct RunOpts {
    pub net: NetOpts,
    /// 正文短于它就不入 RAG（摘要兜底用 0）
    pub min_len_page: usize,
    pub max_feed_items: usize,
}

impl RunOpts {
    pub fn new() -> RunOpts {
        return RunOpts { net: NetOpts::new(), min_len_page: 40, max_feed_items: 8 };
    }
}

/// 一次刷新的结果（可序列化，供 --json / GUI IPC）。
#[derive(Clone, Serialize)]
pub struct TopicRun {
    pub query: String,
    pub key: String,
    pub candidates: usize,
    pub unique: usize,
    pub fetched: usize,
    pub stored: usize,
    pub skipped: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub ms: u64,
    pub status: String,
    pub error: String,
    pub reports: Vec<EngineReport>,
}

/// 一次查询的完整答复。
#[derive(Clone, Serialize)]
pub struct ServeResult {
    pub query: String,
    pub key: String,
    /// new | hit | stale
    pub state: String,
    /// hits 是否完全来自 RAG（true = 这次没联网）
    pub from_cache: bool,
    pub age_ms: u64,
    pub hits: Vec<KbHit>,
    pub run: Option<TopicRun>,
    pub topic: Topic,
    /// 需要调用方后台刷新的 key（Action::Background 时）
    pub pending: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    SearchNow,
    Background,
    CacheOnly,
}

#[derive(Clone, Copy)]
pub struct Policy {
    pub on_new: Action,
    pub on_stale: Action,
}

impl Policy {
    pub fn cli() -> Policy {
        return Policy { on_new: Action::SearchNow, on_stale: Action::SearchNow };
    }
    pub fn gui() -> Policy {
        return Policy { on_new: Action::SearchNow, on_stale: Action::Background };
    }
    pub fn cache_only() -> Policy {
        return Policy { on_new: Action::CacheOnly, on_stale: Action::CacheOnly };
    }
}

// ---------------------------------------------------------------- 检索 ----

/// 查询特征：ASCII 词（>=2 字）+ CJK bigram（+ 单字 run）。
pub fn query_features(query: &str) -> Vec<String> {
    let low = query.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if word.chars().count() >= 2 {
            if seen.insert(word.clone()) { out.push(word.clone()); }
        }
        word.clear();
    };
    let flush_cjk = |cjk: &mut Vec<char>, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if cjk.len() == 1 {
            let s: String = cjk.iter().collect();
            if seen.insert(s.clone()) { out.push(s); }
        } else if cjk.len() >= 2 {
            for i in 0..cjk.len() - 1 {
                let s: String = cjk[i..i + 2].iter().collect();
                if seen.insert(s.clone()) { out.push(s); }
            }
        }
        cjk.clear();
    };
    for c in low.chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut out, &mut seen);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out, &mut seen);
            word.push(c);
        } else {
            flush_word(&mut word, &mut out, &mut seen);
            flush_cjk(&mut cjk, &mut out, &mut seen);
        }
    }
    flush_word(&mut word, &mut out, &mut seen);
    flush_cjk(&mut cjk, &mut out, &mut seen);
    if out.len() > 32 { out.truncate(32); }
    return out;
}

fn is_cjk(c: char) -> bool {
    let v = c as u32;
    return (v >= 0x4E00 && v <= 0x9FFF) || (v >= 0x3400 && v <= 0x4DBF) || (v >= 0xF900 && v <= 0xFAFF)
        || (v >= 0x3040 && v <= 0x30FF) || (v >= 0xAC00 && v <= 0xD7AF);
}

/// 文档相关性：标题命中 3 分 / 正文命中 1 分（bigram+词特征）。
pub fn relevance(title: &str, text: &str, feats: &[String]) -> usize {
    let t = title.to_lowercase();
    let b = text.to_lowercase();
    let mut score = 0usize;
    for f in feats {
        if t.contains(f.as_str()) { score += 3; }
        else if b.contains(f.as_str()) { score += 1; }
    }
    return score;
}

/// RAG 检索：阶段 G BM25 为主，阶段 H 向量模型已配置时做 cosine 融合。
pub fn retrieve(store: &mut Store, query: &str, limit: usize) -> Vec<KbHit> {
    let feats = query_features(query);
    let want = limit.saturating_mul(2).max(limit).max(8);
    let mut by_url: HashMap<String, KbHit> = HashMap::new();
    if feats.is_empty() {
        for h in store.search(query, want) {
            by_url.insert(h.url.clone(), h);
        }
    } else {
        let needle: String = feats[0].clone();
        let bm25 = {
            let idx = store.search_index();
            idx.search(query, want)
        };
        for h in &bm25 {
            let score = (h.score * 100.0).round() as usize;
            if score == 0 { continue; }
            let d = store.search_index().doc(h.doc);
            let hit = KbHit {
                url: d.url.clone(),
                title: d.title.clone(),
                host: d.host.clone(),
                source: d.source.clone(),
                score: score,
                snippet: make_snippet(&d.text, &needle, 160),
                stored_at_ms: d.stored_at_ms,
            };
            by_url.insert(hit.url.clone(), hit);
        }
    }
    let vec_hits = store.vector_candidates(query, limit.saturating_mul(3).max(20));
    let mut out: Vec<KbHit> = by_url.into_values().collect();
    if !vec_hits.is_empty() {
        for (d, vs) in vec_hits {
            if vs <= 0.03 { continue; }
            let vscore = (vs * 10000.0).round() as usize;
            let mut merged = false;
            for h in out.iter_mut() {
                if h.url == d.url {
                    h.score = h.score.saturating_add(vscore);
                    merged = true;
                    break;
                }
            }
            if !merged {
                out.push(KbHit {
                    url: d.url.clone(),
                    title: d.title.clone(),
                    host: d.host.clone(),
                    source: d.source.clone(),
                    score: vscore,
                    snippet: make_snippet(&d.text, &feats.first().cloned().unwrap_or_else(|| query.to_string()), 160),
                    stored_at_ms: d.stored_at_ms,
                });
            }
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.stored_at_ms.cmp(&a.stored_at_ms)));
    out.truncate(limit);
    return out;
}


// ------------------------------------------------------------ 任务清单 ----

/// 建立任务（已存在则只更新参数，不重置运行时间）。
pub fn ensure_topic(store: &mut Store, spec: &TopicSpec, now: u64) -> Topic {
    let key = state::topic_key(&spec.query);
    if let Some(i) = state::find_topic(store.state(), &spec.query) {
        let mut t = store.state().topics[i].clone();
        t.engine = spec.engine.clone();
        t.site = spec.site.clone();
        t.max = spec.max;
        t.fetch_n = spec.fetch_n;
        t.interval_secs = spec.interval_secs;
        t.fresh_secs = spec.fresh_secs;
        state::fold_topic(store.state_mut(), t.clone());
        store.persist_state();
        return t;
    }
    let mut t = default_topic();
    t.query = spec.query.clone();
    t.key = key;
    t.engine = spec.engine.clone();
    t.site = spec.site.clone();
    t.max = spec.max;
    t.fetch_n = spec.fetch_n;
    t.interval_secs = spec.interval_secs;
    t.fresh_secs = spec.fresh_secs;
    t.created_ms = now;
    t.last_run_ms = 0;
    t.next_due_ms = now; // 立刻到期 -> 立即搜索
    state::fold_topic(store.state_mut(), t.clone());
    store.persist_state();
    return t;
}

/// 当前到期的任务 key（顺序 = 入库顺序）。
pub fn due_keys(store: &Store, now: u64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in store.state().topics.iter() {
        if state::is_due(t, now) { out.push(t.key.clone()); }
    }
    return out;
}

/// 把一个主题的时间回拨（验证"太旧 -> 后台再搜"用）。
pub fn backdate_topic(store: &mut Store, q: &str, secs: u64) -> bool {
    let now = now_ms();
    let back = secs.saturating_mul(1000);
    let mut ok = false;
    if let Some(t) = state::find_topic(store.state(), q) {
        let mut t = store.state().topics[t].clone();
        t.last_run_ms = now.saturating_sub(back);
        t.next_due_ms = now;
        state::fold_topic(store.state_mut(), t);
        ok = true;
    }
    if ok { store.persist_state(); }
    return ok;
}

// ---------------------------------------------------------------- 抓取 ----

struct PageDoc {
    url: String,
    title: String,
    text: String,
    source: String,
    min_len: usize,
}

struct Page {
    docs: Vec<PageDoc>,
    status: usize,
    not_modified: bool,
    error: String,
}

fn sniffs_html(body: &str) -> bool {
    let head: String = body.chars().take(2048).collect();
    let low = head.to_lowercase();
    if low.contains("<!doctype html") || low.contains("<html") || low.contains("<body") { return true; }
    return low.contains("<meta ") && low.contains("<div");
}

/// 抓一页正文（HTML 抽取 / RSS 条目 / 纯文本），带条件 GET。
fn fetch_page(url: &str, o: &RunOpts, etag: &str, last_modified: &str) -> Page {
    let mut opt = default_opts();
    opt.user_agent = engines::BROWSER_UA.to_string();
    opt.accept = "text/html,application/xhtml+xml,application/xml;q=0.9,application/rss+xml;q=0.9,*/*;q=0.5".to_string();
    opt.accept_language = "zh-CN,zh;q=0.9,en;q=0.5".to_string();
    opt.timeout_s = o.net.timeout_s;
    opt.retries = o.net.retries;
    opt.max_body = 4 * 1024 * 1024;
    opt.if_none_match = etag.to_string();
    opt.if_modified_since = last_modified.to_string();
    let rep = fetch(url, &opt);
    if !rep.ok {
        return Page { docs: Vec::new(), status: rep.status, not_modified: false, error: rep.error.clone() };
    }
    if rep.not_modified {
        return Page { docs: Vec::new(), status: rep.status, not_modified: true, error: String::new() };
    }
    let final_url = if rep.final_url.is_empty() { url.to_string() } else { rep.final_url.clone() };
    let ct = rep.content_type.to_lowercase();
    let mut docs: Vec<PageDoc> = Vec::new();
    if ct.contains("xml") || ct.contains("rss") || ct.contains("atom") {
        match parse_feed(&rep.body) {
            Ok(f) => {
                let mut n = 0usize;
                for a in f.items.iter() {
                    if n >= o.max_feed_items { break; }
                    if a.url.is_empty() || a.body.trim().is_empty() { continue; }
                    n += 1;
                    docs.push(PageDoc {
                        url: a.url.clone(),
                        title: a.title.clone(),
                        text: a.body.clone(),
                        source: "research-feed".to_string(),
                        min_len: 0,
                    });
                }
            }
            Err(e) => { return Page { docs: Vec::new(), status: rep.status, not_modified: false, error: format!("feed parse: {}", e) }; }
        }
    } else if ct.contains("html") || sniffs_html(&rep.body) {
        let ex = extract(&rep.body);
        docs.push(PageDoc { url: final_url, title: ex.title, text: ex.text, source: "research-page".to_string(), min_len: o.min_len_page });
    } else {
        docs.push(PageDoc { url: final_url, title: String::new(), text: rep.body.clone(), source: "research-text".to_string(), min_len: o.min_len_page });
    }
    return Page { docs: docs, status: rep.status, not_modified: false, error: String::new() };
}

// ---------------------------------------------------------------- 刷新 ----

/// 刷新一个主题：search -> 归一/去重/相关性排序 -> 抓前 fetch_n 页正文 -> 入 RAG -> 记任务。
pub fn refresh(store: &mut Store, key: &str, o: &RunOpts, now: u64) -> Result<TopicRun, String> {
    let idx = match state::find_topic(store.state(), key) {
        Some(i) => i,
        None => { return Err(format!("no such topic: {}", key)); }
    };
    let t = store.state().topics[idx].clone();
    let started = now_ms();
    let mut run = TopicRun {
        query: t.query.clone(),
        key: t.key.clone(),
        candidates: 0,
        unique: 0,
        fetched: 0,
        stored: 0,
        skipped: 0,
        unchanged: 0,
        failed: 0,
        ms: 0,
        status: "ok".to_string(),
        error: String::new(),
        reports: Vec::new(),
    };
    let plan = query::build_plan(&[t.query.clone()], &t.site, &t.engine);
    let (cands, reps) = engines::collect(&plan, t.max, &o.net);
    run.candidates = cands.len();
    run.reports = reps;
    let good: Vec<Candidate> = cands.into_iter().filter(|c| c.resolved && !c.url.is_empty()).collect();
    let mut uniq = normalize::dedup(good);
    run.unique = uniq.len();
    // 相关性排序（稳定排序：同分保持引擎顺序）；这样"卡"字词典页会排在"卡普空"正文之后
    let feats = query_features(&t.query);
    uniq.sort_by(|a, b| {
        let sa = relevance(&a.title, &a.snippet, &feats);
        let sb = relevance(&b.title, &b.snippet, &feats);
        return sb.cmp(&sa);
    });
    let mut docs: Vec<String> = Vec::new();
    for c in uniq.iter().take(t.fetch_n) {
        // 条件 GET 用的 etag 来自 F3 状态文件
        let (etag, lm) = match state::find(store.state(), &c.url) {
            Some(e) => (e.etag.clone(), e.last_modified.clone()),
            None => (String::new(), String::new()),
        };
        let page = fetch_page(&c.url, o, &etag, &lm);
        run.fetched += 1;
        let _ = store.record_fetch(&c.url, "research", page.status as u64, &etag, &lm);
        if !page.error.is_empty() {
            run.failed += 1;
            if run.error.is_empty() { run.error = page.error.clone(); }
        }
        if page.not_modified { run.unchanged += 1; }
        let mut batch: Vec<PageDoc> = page.docs;
        if batch.is_empty() && !page.not_modified && !c.snippet.trim().is_empty() {
            // 页面抓不到：至少留下 SERP 摘要（source=research-serp）
            batch.push(PageDoc { url: c.url.clone(), title: c.title.clone(), text: c.snippet.clone(), source: "research-serp".to_string(), min_len: 0 });
        }
        for d in batch {
            match store.save(&d.url, &d.title, &d.text, &t.query, &d.source, d.min_len) {
                Ok(msg) => {
                    if let Some(rest) = msg.strip_prefix("stored: ") {
                        run.stored += 1;
                        let h = rest.split(' ').next().unwrap_or("").to_string();
                        if !h.is_empty() { docs.push(h); }
                    } else {
                        run.skipped += 1;
                    }
                }
                Err(e) => {
                    run.failed += 1;
                    if run.error.is_empty() { run.error = e; }
                }
            }
        }
    }
    let end = now_ms();
    run.ms = end.saturating_sub(started);
    // 只有"全部候选页都没抓到"或"所有引擎都失败"才算 error；个别页失败记 last_error 但保持 ok
    let engines_all_failed = !run.reports.is_empty() && run.reports.iter().all(|r| !r.ok);
    if engines_all_failed || (run.fetched > 0 && run.failed >= run.fetched) {
        run.status = "error".to_string();
    }
    let _ = now;
    let iv = t.interval_secs.saturating_mul(1000);
    if let Some(slot) = state::find_topic(store.state(), &t.key) {
        let st = store.state_mut();
        let tt = &mut st.topics[slot];
        tt.last_run_ms = end;
        tt.next_due_ms = end.saturating_add(iv);
        tt.runs += 1;
        tt.stored += run.stored as u64;
        // 本轮没有新文档时保留上一次的 docs（任务清单更有用）
        if !docs.is_empty() { tt.docs = docs; }
        tt.last_status = run.status.clone();
        tt.last_error = run.error.clone();
    }
    store.persist_state();
    return Ok(run);
}

/// 只搜候选 + 去重 + 相关性排序，取前 fetch_n 的 URL（供 GUI 用浏览器逐页爬）。
/// 不抓取、不入库：把"抓正文"交给浏览器窗口（能过反爬），这里只产出有序的候选链接。
pub fn candidate_urls(store: &Store, key: &str, o: &RunOpts) -> Result<Vec<String>, String> {
    let idx = match state::find_topic(store.state(), key) {
        Some(i) => i,
        None => { return Err(format!("no such topic: {}", key)); }
    };
    let t = store.state().topics[idx].clone();
    let plan = query::build_plan(&[t.query.clone()], &t.site, &t.engine);
    let (cands, _reps) = engines::collect(&plan, t.max, &o.net);
    let good: Vec<Candidate> = cands.into_iter().filter(|c| c.resolved && !c.url.is_empty()).collect();
    let mut uniq = normalize::dedup(good);
    let feats = query_features(&t.query);
    uniq.sort_by(|a, b| {
        let sa = relevance(&a.title, &a.snippet, &feats);
        let sb = relevance(&b.title, &b.snippet, &feats);
        return sb.cmp(&sa);
    });
    let urls: Vec<String> = uniq.iter().take(t.fetch_n).map(|c| c.url.clone()).collect();
    return Ok(urls);
}

/// 浏览器爬完一轮后更新任务（runs / stored / 下次到期）并落盘。
pub fn finalize_crawl(store: &mut Store, key: &str, stored: u64, now: u64) {
    if let Some(slot) = state::find_topic(store.state(), key) {
        let st = store.state_mut();
        let tt = &mut st.topics[slot];
        let iv = tt.interval_secs.saturating_mul(1000);
        tt.last_run_ms = now;
        tt.next_due_ms = now.saturating_add(iv);
        tt.runs += 1;
        tt.stored += stored;
        tt.last_status = "ok".to_string();
    }
    store.persist_state();
}

/// 到期任务逐个刷新（顺序执行，避免对同一站点并发过猛）。
pub fn refresh_due(store: &mut Store, o: &RunOpts, now: u64) -> Vec<TopicRun> {
    let mut out: Vec<TopicRun> = Vec::new();
    for key in due_keys(store, now) {
        match refresh(store, &key, o, now) {
            Ok(r) => out.push(r),
            Err(e) => {
                if let Some(i) = state::find_topic(store.state(), &key) {
                    let st = store.state_mut();
                    st.topics[i].last_status = "error".to_string();
                    st.topics[i].last_error = e;
                    st.topics[i].next_due_ms = now + 60_000;
                }
                let _ = store.persist_state();
            }
        }
    }
    return out;
}

/// 缓存优先答复（用户原则的入口）。
pub fn handle_query(store: &mut Store, spec: &TopicSpec, o: &RunOpts, policy: &Policy, now: u64) -> Result<ServeResult, String> {
    let key = state::topic_key(&spec.query);
    let existed = state::find_topic(store.state(), &spec.query).is_some();
    // 先按本次参数更新任务（--fresh / --interval 立即生效），再判断新鲜度。
    let topic = ensure_topic(store, spec, now);
    let mut state_name = "new";
    let mut from_cache = true;
    let mut age_ms = 0u64;
    let mut run: Option<TopicRun> = None;
    let mut pending: Option<String> = None;
    let mut action = policy.on_new;
    if existed {
        age_ms = now.saturating_sub(topic.last_run_ms);
        let fresh = topic.runs > 0 && age_ms <= topic.fresh_secs.saturating_mul(1000);
        if fresh {
            state_name = "hit";
            // 命中：绝不联网（用户原则 2）
            action = Action::CacheOnly;
        } else {
            state_name = "stale";
            action = policy.on_stale;
        }
    }
    match action {
        Action::SearchNow => {
            let r = refresh(store, &key, o, now)?;
            run = Some(r);
            from_cache = false;
        }
        Action::Background => {
            pending = Some(key.clone());
        }
        Action::CacheOnly => {}
    }
    let hits = retrieve(store, &topic.query, topic.max);
    let topic = match state::find_topic(store.state(), &key) {
        Some(i) => store.state().topics[i].clone(),
        None => topic,
    };
    return Ok(ServeResult {
        query: spec.query.clone(),
        key: key,
        state: state_name.to_string(),
        from_cache: from_cache,
        age_ms: age_ms,
        hits: hits,
        run: run,
        topic: topic,
        pending: pending,
    });
}

// ------------------------------------------------------------ 常驻调度 ----

fn inflight() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    return SET.get_or_init(|| Mutex::new(HashSet::new()));
}

/// 后台刷新一个主题（同 key 同时只跑一个）。返回 false = 已有在跑。
pub fn spawn_refresh(store: Arc<Mutex<Store>>, key: String, o: RunOpts, log: Box<dyn Fn(String) + Send>) -> bool {
    {
        let mut set = match inflight().lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if set.contains(&key) { return false; }
        set.insert(key.clone());
    }
    std::thread::spawn(move || {
        let now = now_ms();
        let res = {
            let mut g = match store.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            refresh(&mut g, &key, &o, now)
        };
        match res {
            Ok(r) => log(format!("[topic] {} 刷新完成: 候选 {} / 入库 {} / 跳过 {} / 未变 {} / 失败 {} ({} ms)",
                r.query, r.unique, r.stored, r.skipped, r.unchanged, r.failed, r.ms)),
            Err(e) => log(format!("[topic] {} 刷新失败: {}", key, e)),
        }
        let mut set = match inflight().lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        set.remove(&key);
    });
    return true;
}

/// 常驻调度：每 tick 检查到期任务并刷新（GUI 一直运行 -> 一直间隔取最新）。
pub fn spawn_scheduler(store: Arc<Mutex<Store>>, o: RunOpts, tick_secs: u64, stop: Arc<AtomicBool>, log: Box<dyn Fn(String) + Send>) -> std::thread::JoinHandle<()> {
    let tick = if tick_secs == 0 { DEFAULT_SCHED_TICK_SECS } else { tick_secs };
    return std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(tick));
            if stop.load(Ordering::SeqCst) { break; }
            let now = now_ms();
            let runs = {
                let mut g = match store.lock() {
                    Ok(g) => g,
                    Err(e) => e.into_inner(),
                };
                refresh_due(&mut g, &o, now)
            };
            for r in runs {
                log(format!("[topic] {} 定时刷新: 候选 {} / 入库 {} / 状态 {}", r.query, r.unique, r.stored, r.status));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::query_features;
    use super::relevance;
    use super::retrieve;
    use super::Action;
    use super::Policy;
    use super::RunOpts;
    use super::TopicSpec;
    use crate::store::kb::Store;
    use std::path::PathBuf;

    fn kb(name: &str) -> PathBuf {
        let p = PathBuf::from("_scratch").join(name);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(crate::store::kb::default_state_path(&p));
        return p;
    }

    #[test]
    fn features_and_relevance() {
        let f = query_features("卡普空 最新游戏 Rust 1.9");
        assert!(f.contains(&"卡普".to_string()));
        assert!(f.contains(&"普空".to_string()));
        assert!(f.contains(&"rust".to_string()));
        assert!(!f.contains(&"1".to_string()));
        let score = relevance("卡普空最新作发布", "游戏内容", &f);
        assert!(score >= 3);
        let zero = relevance("英汉词典", "拼音部首笔画", &f);
        assert_eq!(zero, 0);
    }

    #[test]
    fn retrieve_ranks_title_above_body() {
        let p = kb("f_research_retrieve.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/", "卡普空新作", "正文提到 卡普空 一次", "", "manual", 0).unwrap();
        s.save("https://b.example.com/", "普通标题", "正文里出现 卡普空 但没有标题命中，比标题命中分低", "", "manual", 0).unwrap();
        s.save("https://c.example.com/", "无关", "完全无关的正文", "", "manual", 0).unwrap();
        let hits = retrieve(&mut s, "卡普空最新游戏", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://a.example.com/");
        assert!(hits[0].score > hits[1].score);
        let _ = std::fs::remove_file("_scratch/f_research_retrieve.state.toml");
    }

    #[test]
    fn handle_query_new_hit_stale_without_network() {
        let p = kb("f_research_serve.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://news.example.com/capcom", "卡普空公布新作", "怪物猎人 最新作 卡普空 发布会", "", "manual", 0).unwrap();
        let spec = TopicSpec::new("卡普空最新游戏");
        let o = RunOpts::new();
        let now = crate::store::doc::now_ms();
        // cache_only：不联网，只建任务
        let r1 = super::handle_query(&mut s, &spec, &o, &Policy::cache_only(), now).unwrap();
        assert_eq!(r1.state, "new");
        assert_eq!(r1.pending.is_none(), true);
        assert_eq!(s.state().topics.len(), 1);
        assert_eq!(s.state().topics[0].query, "卡普空最新游戏");
        // 记一次"刚跑过" -> 命中
        {
            let st = s.state_mut();
            let t = &mut st.topics[0];
            t.runs = 1;
            t.last_run_ms = now;
            t.next_due_ms = now + 600_000;
        }
        let r2 = super::handle_query(&mut s, &spec, &o, &Policy::cache_only(), now + 1000).unwrap();
        assert_eq!(r2.state, "hit");
        assert_eq!(r2.from_cache, true);
        assert!(r2.hits.len() >= 1);
        assert_eq!(r2.run.is_none(), true);
        // 回归：命中时即使策略是 SearchNow 也不能联网刷新（曾误把 action 留成 on_new）
        let runs_before = s.state().topics[0].runs;
        let r2b = super::handle_query(&mut s, &spec, &o, &Policy::cli(), now + 1500).unwrap();
        assert_eq!(r2b.state, "hit");
        assert_eq!(r2b.from_cache, true);
        assert_eq!(r2b.run.is_none(), true, "命中必须走缓存，不触发联网刷新");
        assert_eq!(s.state().topics[0].runs, runs_before, "命中不得增加 runs");
        // 太旧 -> stale（策略 CacheOnly 下不联网，但仍返回旧数据）
        assert_eq!(super::backdate_topic(&mut s, "卡普空最新游戏", 7 * 3600), true);
        let r3 = super::handle_query(&mut s, &spec, &o, &Policy::cache_only(), now + 2000).unwrap();
        assert_eq!(r3.state, "stale");
        assert_eq!(r3.from_cache, true);
        assert!(r3.age_ms > 6 * 3600 * 1000);
        assert!(r3.hits.len() >= 1, "stale 也要先把 RAG 旧数据返回");
        assert_eq!(r3.run.is_none(), true, "CacheOnly 不联网");
        // Background 策略：stale 时应给出 pending key
        assert_eq!(super::backdate_topic(&mut s, "卡普空最新游戏", 7 * 3600), true);
        let p2 = Policy { on_new: Action::SearchNow, on_stale: Action::Background };
        let r4 = super::handle_query(&mut s, &spec, &o, &p2, now + 3000).unwrap();
        assert_eq!(r4.state, "stale");
        assert_eq!(r4.pending.is_some(), true);
        assert_eq!(r4.from_cache, true);
        // 到期清单
        let due = super::due_keys(&s, now + 100_000_000);
        assert_eq!(due.len(), 1);
        let _ = std::fs::remove_file("_scratch/f_research_serve.state.toml");
    }
}
