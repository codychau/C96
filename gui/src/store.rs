//! F/I：GUI 侧存储桥
//!   F：委托根 crate 的 c96::store（单一知识库 + 30 天 TTL + 指纹去重）
//!   I：委托 c96::research（主题订阅：首搜即入库 / 命中读 RAG / 过期后台刷新 / 常驻轮询）

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use c96::research;
use c96::store::kb;
use tauri::AppHandle;
use tauri::Manager;

use crate::browser;

pub use c96::research::ServeResult;
pub use c96::research::TopicRun;
pub use c96::store::doc::KbDoc;
pub use c96::store::kb::KbHit;
pub use c96::store::kb::StoreStats as KbStats;
pub use c96::store::state::Topic;

static STORE: OnceLock<Arc<Mutex<kb::Store>>> = OnceLock::new();

/// kb.jsonl 路径：Tauri app_data_dir（双击启动时 CWD 不可靠）。
pub fn kb_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    return Ok(dir.join("kb.jsonl"));
}

/// 进程内唯一 Store 句柄（Arc：后台调度线程也要持有）。
pub fn handle(app: &AppHandle) -> Result<Arc<Mutex<kb::Store>>, String> {
    if STORE.get().is_none() {
        let path = kb_path(app)?;
        let opened = kb::Store::open(&path, kb::DEFAULT_TTL_DAYS)?;
        let _ = STORE.set(Arc::new(Mutex::new(opened)));
    }
    return STORE.get().cloned().ok_or_else(|| "store init failed".to_string());
}

fn with<T>(app: &AppHandle, f: impl FnOnce(&mut kb::Store) -> T) -> Result<T, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    return Ok(f(&mut g));
}

/// 主题刷新用的网络参数（GUI 侧固定保守值）。
pub fn run_opts() -> research::RunOpts {
    let mut o = research::RunOpts::new();
    o.net.timeout_s = 20;
    o.net.retries = 1;
    return o;
}

/// 用浏览器逐页爬取一个已存在的订阅任务：搜候选 → 串行导航进窗口抽正文入库 → 更新任务。
pub fn crawl_topic(app: &AppHandle, query: &str) -> Result<String, String> {
    let arc = handle(app)?;
    let urls = {
        let g = arc.lock().unwrap_or_else(|e| e.into_inner());
        research::candidate_urls(&g, query, &run_opts())?
    };
    if urls.is_empty() { return Ok("没有候选可爬".to_string()); }
    let rep = browser::crawl(app, &urls, query);
    {
        let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
        research::finalize_crawl(&mut g, query, rep.stored, c96::store::doc::now_ms());
    }
    let msg = format!("浏览器爬取 {} 页 · 入库 {} · 跳过 {} · 失败 {}",
        urls.len(), rep.stored, rep.skipped, rep.failed);
    if let Ok(p) = std::env::var("C96_CRAWL_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "[crawl_topic] {} => {} (urls={})", query, msg, urls.len());
        }
    }
    return Ok(msg);
}

// ------------------------------------------------------------------ F ----

/// 保存一页正文（E3 自动入库路径，source=gui-browser）。
pub fn save(app: &AppHandle, url: &str, title: &str, text: &str, query: &str, min_len: usize) -> Result<String, String> {
    return with(app, |s| s.save(url, title, text, query, "gui-browser", min_len))?;
}

/// E7：GUI 浏览器自动入库（带正文抽取模式）。重访同一 URL 时，如果本次抽取比旧记录干净
/// （模式升级，或正文明显变长），Store 会用新正文替换旧记录 —— 历史上「整页 raw（含菜单）」
/// 的条目因此能被一次干净重访自动修好，而不是被 URL 去重永远挡住。
pub fn save_capture(app: &AppHandle, url: &str, title: &str, text: &str, query: &str, min_len: usize, mode: &str) -> Result<String, String> {
    return with(app, |s| s.save_refresh(url, title, text, query, "gui-browser", min_len, mode))?;
}

pub fn stats(app: &AppHandle) -> Result<KbStats, String> {
    return with(app, |s| s.stats());
}

pub fn search(app: &AppHandle, query: &str, limit: usize) -> Result<Vec<KbHit>, String> {
    return with(app, |s| s.search(query, limit));
}

pub fn last(app: &AppHandle) -> Option<KbDoc> {
    let r = with(app, |s| s.last());
    match r {
        Ok(v) => return v,
        Err(_) => return None,
    }
}

/// F：显式过期清理（open 已自动清理；供 IPC / 自检再跑一次）。
pub fn purge(app: &AppHandle) -> Result<usize, String> {
    return with(app, |s| s.purge());
}

/// F 自检：注入一条 backdated 记录（验证 TTL 自动丢弃）。
pub fn inject_backdated(app: &AppHandle, url: &str, title: &str, text: &str, query: &str, days: u64) -> Result<String, String> {
    return with(app, |s| s.add_backdated(url, title, text, query, "gui-ttl-probe", 0, days))?;
}

// ------------------------------------------------------------------ I ----

/// 主题查询（用户原则入口）：
///   没搜过 -> 立刻搜索 + 建任务（Policy::gui 的 on_new = SearchNow）
///   命中   -> 只读 RAG，不联网
///   太旧   -> 先返回 RAG 旧数据，同时后台再搜一次
pub fn topic_query(app: &AppHandle, query: &str, cache_only: bool) -> Result<ServeResult, String> {
    let spec = research::TopicSpec::new(query);
    let policy = if cache_only { research::Policy::cache_only() } else { research::Policy::gui() };
    let arc = handle(app)?;
    let (res, pending) = {
        let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
        let r = research::handle_query(&mut g, &spec, &run_opts(), &policy, c96::store::doc::now_ms())?;
        let p = r.pending.clone();
        (r, p)
    };
    if let Some(key) = pending {
        let log: Box<dyn Fn(String) + Send> = Box::new(|m| println!("{}", m));
        let spawned = research::spawn_refresh(arc, key, run_opts(), log);
        println!("[topic] 后台刷新 {} (spawned={})", res.query, spawned);
    }
    return Ok(res);
}

pub fn topic_list(app: &AppHandle) -> Result<Vec<Topic>, String> {
    return with(app, |s| s.state().topics.clone());
}

/// 立即刷新一个主题：用浏览器逐页爬（过反爬），返回与旧刷新兼容的 TopicRun。
pub fn topic_run(app: &AppHandle, query: &str) -> Result<TopicRun, String> {
    let arc = handle(app)?;
    let urls = {
        let g = arc.lock().unwrap_or_else(|e| e.into_inner());
        research::candidate_urls(&g, query, &run_opts())?
    };
    let started = c96::store::doc::now_ms();
    let rep = if urls.is_empty() {
        browser::CrawlReport { stored: 0, skipped: 0, failed: 0 }
    } else {
        browser::crawl(app, &urls, query)
    };
    {
        let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
        research::finalize_crawl(&mut g, query, rep.stored, c96::store::doc::now_ms());
    }
    let end = c96::store::doc::now_ms();
    let r = research::TopicRun {
        query: query.to_string(),
        key: c96::store::state::topic_key(query),
        candidates: urls.len(),
        unique: urls.len(),
        fetched: urls.len(),
        stored: rep.stored as usize,
        skipped: rep.skipped as usize,
        unchanged: 0,
        failed: rep.failed as usize,
        ms: end.saturating_sub(started),
        status: "ok".to_string(),
        error: String::new(),
        reports: Vec::new(),
    };
    return Ok(r);
}

pub fn topic_remove(app: &AppHandle, query: &str) -> Result<bool, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    let removed = c96::store::state::remove_topic(g.state_mut(), query);
    let _ = g.persist_state();
    return Ok(removed);
}

/// 把主题时间回拨（验证"太旧 -> 后台再搜"；也可做运维手段）。
pub fn topic_backdate(app: &AppHandle, query: &str, secs: u64) -> Result<bool, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    return Ok(research::backdate_topic(&mut g, query, secs));
}

// ---------------------------------------------------------------- RSS 管理器 ----

/// 管理面板用的一行内容（轻量：不带正文，避免 IPC 传几十 KB）。
/// 元组 = (hash, url, title, host, stored_at_ms, text_len)
pub type DocRow = (String, String, String, String, u64, usize);
/// 来源行 = (名称, 条数, 最近入库 ms, 样例标题)
pub type SourceRow = (String, usize, u64, String);
/// 「全部内容」行 = (hash, url, title, host, source, query, stored_at_ms, text_len)
pub type AnyDocRow = (String, String, String, String, String, String, u64, usize);
/// 主机行 = (host, 条数, 最近入库 ms, 样例标题, 是否已忽略)
pub type HostRow = (String, usize, u64, String, bool);
/// (本机 base, 局域网 base, token)
pub type RssBase = (String, String, String);

static RSS_BASE: OnceLock<(String, String, String)> = OnceLock::new();

/// 启动 RSS 服务器后记录地址（面板用来给订阅链接）。
pub fn set_rss_base(local: &str, lan: &str, token: &str) {
    let _ = RSS_BASE.set((local.to_string(), lan.to_string(), token.to_string()));
}

pub fn rss_base() -> RssBase {
    return RSS_BASE.get().cloned().unwrap_or_else(|| ("".to_string(), "".to_string(), "".to_string()));
}

fn ui_lang_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    return Ok(dir.join("ui-lang.txt"));
}

/// UI 语言：`zh`（默认）/ `en`，落在 app_data_dir，跨站点共享。
pub fn ui_lang(app: &AppHandle) -> String {
    if let Ok(p) = ui_lang_path(app) {
        if let Ok(s) = std::fs::read_to_string(p) {
            return if s.trim() == "en" { "en".to_string() } else { "zh".to_string() };
        }
    }
    return "zh".to_string();
}

pub fn set_ui_lang(app: &AppHandle, lang: &str) -> Result<String, String> {
    let l = if lang == "en" { "en" } else { "zh" };
    let p = ui_lang_path(app)?;
    std::fs::write(p, l).map_err(|e| e.to_string())?;
    return Ok(l.to_string());
}

/// 「改」：改间隔 / 暂停恢复，并重算 next_due。
pub fn topic_update(app: &AppHandle, query: &str, interval_secs: u64, paused: Option<bool>) -> Result<bool, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    let idx = match c96::store::state::find_topic(g.state(), query) {
        Some(i) => i,
        None => { return Ok(false); }
    };
    {
        let st = g.state_mut();
        let t = &mut st.topics[idx];
        if interval_secs > 0 { t.interval_secs = interval_secs; }
        if let Some(p) = paused { t.paused = p; }
        t.next_due_ms = t.last_run_ms.saturating_add(t.interval_secs.saturating_mul(1000));
    }
    let ok = g.persist_state();
    return Ok(ok);
}

/// 「查」：某关键词订阅的最近条目（按 topic_key 过滤，同 RSS 单主题 feed 一致）。
pub fn topic_docs(app: &AppHandle, query: &str, limit: usize) -> Result<Vec<DocRow>, String> {
    let key = c96::store::state::topic_key(query);
    return with(app, |s| {
        let mut v: Vec<DocRow> = Vec::new();
        for d in s.list(500, "") {
            if c96::store::state::topic_key(&d.query) != key { continue; }
            v.push((d.hash.clone(), d.url.clone(), d.title.clone(), d.host.clone(), d.stored_at_ms, d.text_len));
            if v.len() >= limit { break; }
        }
        return v;
    });
}

/// 「查」：来源分组（对应 /rss/source/<name>.xml）。
pub fn sources(app: &AppHandle) -> Result<Vec<SourceRow>, String> {
    return with(app, |s| {
        let mut map: std::collections::BTreeMap<String, (usize, u64, String)> = std::collections::BTreeMap::new();
        for d in s.list(500, "") {
            let name = if d.source.is_empty() { "(none)".to_string() } else { d.source.clone() };
            let e = map.entry(name).or_insert((0, 0, String::new()));
            e.0 += 1;
            if d.stored_at_ms >= e.1 {
                e.1 = d.stored_at_ms;
                e.2 = d.title.clone();
            }
        }
        let mut v: Vec<SourceRow> = Vec::new();
        for (name, tup) in map.into_iter() { v.push((name, tup.0, tup.1, tup.2)); }
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        return v;
    });
}

/// 「删」：删一条内容（按 hash）。
pub fn doc_remove(app: &AppHandle, hash: &str) -> Result<bool, String> {
    return with(app, |s| s.remove_hash(hash));
}

/// 「删」：清空一个来源（source 精确匹配；空串 = 无来源那批）。返回删除条数。
pub fn source_remove(app: &AppHandle, source: &str) -> Result<usize, String> {
    return with(app, |s| s.remove_source(source));
}

/// 「删」：清空一个主机（大小写不敏感）。返回删除条数。
pub fn host_remove(app: &AppHandle, host: &str) -> Result<usize, String> {
    return with(app, |s| s.remove_host(host));
}

/// 「查」：全部内容（最近在前）。filter 为空不过滤；only_unsub 只留「不属于任何现存订阅」的随手收录。
/// 过滤在 Rust 侧做，保证「订阅/随手」判定与 RSS 一致。
pub fn docs_all(app: &AppHandle, filter: &str, only_unsub: bool, limit: usize) -> Result<serde_json::Value, String> {
    let f = filter.trim().to_lowercase();
    return with(app, |s| {
        let total = s.stats().docs;
        let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in s.state().topics.iter() { keys.insert(t.key.clone()); }
        let mut rows: Vec<AnyDocRow> = Vec::new();
        for d in s.list(0, "") {
            if only_unsub {
                let k = c96::store::state::topic_key(&d.query);
                if !k.is_empty() && keys.contains(&k) { continue; }
            }
            if !f.is_empty() {
                let hay = d.title.to_lowercase() + " " + &d.url.to_lowercase() + " " + &d.host.to_lowercase()
                    + " " + &d.source.to_lowercase() + " " + &d.query.to_lowercase();
                if !hay.contains(&f) { continue; }
            }
            rows.push((d.hash.clone(), d.url.clone(), d.title.clone(), d.host.clone(), d.source.clone(), d.query.clone(), d.stored_at_ms, d.text_len));
            if limit > 0 && rows.len() >= limit { break; }
        }
        let mut o = serde_json::Map::new();
        o.insert("total".to_string(), serde_json::json!(total));
        o.insert("shown".to_string(), serde_json::json!(rows.len()));
        o.insert("limit".to_string(), serde_json::json!(limit));
        o.insert("only_unsub".to_string(), serde_json::json!(only_unsub));
        o.insert("rows".to_string(), serde_json::to_value(rows).unwrap_or(serde_json::Value::Array(Vec::new())));
        return serde_json::Value::Object(o);
    });
}

/// 「查」：主机分组（全量，不受过滤影响），带是否已忽略。
pub fn hosts(app: &AppHandle) -> Result<Vec<HostRow>, String> {
    return with(app, |s| {
        let mut map: std::collections::BTreeMap<String, (usize, u64, String)> = std::collections::BTreeMap::new();
        for d in s.list(0, "") {
            let name = if d.host.is_empty() { "(none)".to_string() } else { d.host.clone() };
            let e = map.entry(name).or_insert((0, 0, String::new()));
            e.0 += 1;
            if d.stored_at_ms >= e.1 {
                e.1 = d.stored_at_ms;
                e.2 = d.title.clone();
            }
        }
        let mut v: Vec<HostRow> = Vec::new();
        for (name, tup) in map.into_iter() {
            let raw = if name == "(none)" { String::new() } else { name.clone() };
            v.push((name, tup.0, tup.1, tup.2, c96::store::state::is_ignored(s.state(), &raw)));
        }
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        return v;
    });
}

/// 忽略名单（面板「不再收录这个站」）。
pub fn ignored_hosts(app: &AppHandle) -> Result<Vec<String>, String> {
    return with(app, |s| s.state().ignored_hosts.clone());
}

/// 加/取消忽略；返回状态是否真的变了。
pub fn ignore_host(app: &AppHandle, host: &str, on: bool) -> Result<bool, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    let changed = if on { c96::store::state::ignore_host(g.state_mut(), host) } else { c96::store::state::unignore_host(g.state_mut(), host) };
    if changed { let _ = g.persist_state(); }
    return Ok(changed);
}

/// 用户自加的 URL 黑名单 glob（内置 SERP 规则不在此列表里，但生效时合并）。
pub fn blocked_patterns(app: &AppHandle) -> Result<Vec<String>, String> {
    return with(app, |s| s.state().blocked_patterns.clone());
}

/// 面板用：内置 SERP 规则 + 用户黑名单，一次回传。
pub fn patterns_info(app: &AppHandle) -> Result<serde_json::Value, String> {
    let user = blocked_patterns(app)?;
    let built = c96::store::state::default_blocked_patterns();
    return serde_json::to_value(serde_json::json!({ "built_in": built, "user": user })).map_err(|e| e.to_string());
}

/// 加/删用户黑名单 glob（内置规则不可删）；返回状态是否真的变了。
pub fn pattern_block(app: &AppHandle, pattern: &str, on: bool) -> Result<bool, String> {
    let arc = handle(app)?;
    let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
    let changed = if on { c96::store::state::block_pattern(g.state_mut(), pattern) } else { c96::store::state::unblock_pattern(g.state_mut(), pattern) };
    if changed { let _ = g.persist_state(); }
    return Ok(changed);
}

/// 注入面板的请求分发器（走 WebView2 原生 postMessage 通道，不用 Tauri IPC：
/// 外部页面/ file:// 的 Origin 不被 IPC 接受，实测报 "Origin header is not a valid URL"）。
pub fn rss_op(app: &AppHandle, op: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let sarg = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let uarg = |k: &str| args.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let barg = |k: &str| args.get(k).and_then(|v| v.as_bool());
    match op {
        "lang_get" => { return Ok(serde_json::json!(ui_lang(app))); }
        "lang_set" => { return Ok(serde_json::json!(set_ui_lang(app, &sarg("lang"))?)); }
        "base" => { return serde_json::to_value(rss_base()).map_err(|e| e.to_string()); }
        "topics" => { return serde_json::to_value(topic_list(app)?).map_err(|e| e.to_string()); }
        "sources" => { return serde_json::to_value(sources(app)?).map_err(|e| e.to_string()); }
        "topic_docs" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            return serde_json::to_value(topic_docs(app, &sarg("query"), limit)?).map_err(|e| e.to_string());
        }
        "topic_add" => {
            // 增：立刻搜索入库（用户原则 1），再设间隔
            let q = sarg("query");
            let r = topic_query(app, &q, false)?;
            let interval = uarg("interval_secs");
            if interval > 0 { let _ = topic_update(app, &q, interval, Some(false)); }
            let mut o = serde_json::Map::new();
            o.insert("hits".to_string(), serde_json::json!(r.hits.len()));
            o.insert("stored".to_string(), serde_json::json!(r.topic.stored));
            o.insert("state".to_string(), serde_json::json!(r.state));
            return Ok(serde_json::Value::Object(o));
        }
        "topic_update" => {
            return serde_json::to_value(topic_update(app, &sarg("query"), uarg("interval_secs"), barg("paused"))?).map_err(|e| e.to_string());
        }
        "topic_run" => { return serde_json::to_value(topic_run(app, &sarg("query"))?).map_err(|e| e.to_string()); }
        "topic_remove" => { return serde_json::to_value(topic_remove(app, &sarg("query"))?).map_err(|e| e.to_string()); }
        "doc_remove" => { return serde_json::to_value(doc_remove(app, &sarg("hash"))?).map_err(|e| e.to_string()); }
        "source_remove" => { return serde_json::to_value(source_remove(app, &sarg("source"))?).map_err(|e| e.to_string()); }
        "host_remove" => { return serde_json::to_value(host_remove(app, &sarg("host"))?).map_err(|e| e.to_string()); }
        "docs" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(300) as usize;
            return docs_all(app, &sarg("filter"), args.get("only_unsub").and_then(|v| v.as_bool()).unwrap_or(false), limit);
        }
        "hosts" => { return serde_json::to_value(hosts(app)?).map_err(|e| e.to_string()); }
        "ignored" => { return serde_json::to_value(ignored_hosts(app)?).map_err(|e| e.to_string()); }
        "ignore" => {
            let on = args.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
            return serde_json::to_value(ignore_host(app, &sarg("host"), on)?).map_err(|e| e.to_string());
        }
        "patterns" => { return patterns_info(app); }
        "pattern_add" => { return serde_json::to_value(pattern_block(app, &sarg("pattern"), true)?).map_err(|e| e.to_string()); }
        "pattern_remove" => { return serde_json::to_value(pattern_block(app, &sarg("pattern"), false)?).map_err(|e| e.to_string()); }
        _ => { return Err("unknown rss op: ".to_string() + op); }
    }
}

