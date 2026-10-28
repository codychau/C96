//! C96 阶段 E — Tauri v2 GUI：默认双击（无参数）= 浏览器主窗口 + 搜索条 + 自动 RAG 采集。
//!
//! 独立 crate（gui/），不进入根 C96 的零依赖核心构建。
//! 主窗口直接加载外部网页，页面里注入 C96 搜索条（搜内容/网址），
//! 每次页面加载后把正文自动写入本地 RAG 知识库（store.rs，JSONL + URL 去重）。
//!
//! 附带：--selftest（E1/E2 裸 WebView2 + 事件）、--selftest-rag（E3 自动入库）、
//!      --selftest-topics（I 主题订阅）、--selftest-links（链接点击：target=_blank / window.open）、
//!      --selftest-extract（E7 正文抽取：菜单/侧栏/推荐不进门，链接堆页被闸门拦住）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod store;

use tauri::Manager;

const BANNER: &str = "C96 — 资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库";
const START_URL: &str = "https://www.bing.com/";

#[tauri::command]
fn banner() -> String {
    return BANNER.to_string() + " · GUI (E1+E2+E3, Tauri v2 + WebView2)";
}

#[tauri::command]
fn open_browser(app: tauri::AppHandle, url: String) -> Result<String, String> {
    return browser::open(&app, &url);
}

#[tauri::command]
fn snapshot_probe(app: tauri::AppHandle) -> Result<String, String> {
    return browser::probe(&app);
}

#[tauri::command]
fn webview2_info(app: tauri::AppHandle, label: Option<String>) -> Result<String, String> {
    let l = label.unwrap_or_else(|| "browser".to_string());
    return browser::raw_info(&app, &l);
}

#[tauri::command]
fn webview2_zoom(app: tauri::AppHandle, label: Option<String>, factor: f64) -> Result<String, String> {
    let l = label.unwrap_or_else(|| "browser".to_string());
    return browser::raw_zoom(&app, &l, factor);
}

#[tauri::command]
fn webview2_navigate(app: tauri::AppHandle, label: Option<String>, url: String) -> Result<String, String> {
    let l = label.unwrap_or_else(|| "browser".to_string());
    return browser::raw_navigate(&app, &l, &url);
}

/// RSS 内容管理器（注入的顶部工具栏面板）用的命令：增 / 删 / 改 / 查。
#[tauri::command]
fn topic_update(app: tauri::AppHandle, query: String, interval_secs: u64, paused: Option<bool>) -> Result<bool, String> {
    return store::topic_update(&app, &query, interval_secs, paused);
}

/// 查：某关键词订阅最近的条目（轻量行，不带正文）。
#[tauri::command]
fn topic_docs(app: tauri::AppHandle, query: String, limit: Option<usize>) -> Result<Vec<store::DocRow>, String> {
    return store::topic_docs(&app, &query, limit.unwrap_or(20));
}

/// 查：来源分组（/rss/source/<name>.xml）。
#[tauri::command]
fn rss_sources(app: tauri::AppHandle) -> Result<Vec<store::SourceRow>, String> {
    return store::sources(&app);
}

/// 删：删一条内容（按 hash）。
#[tauri::command]
fn rss_doc_remove(app: tauri::AppHandle, hash: String) -> Result<bool, String> {
    return store::doc_remove(&app, &hash);
}

/// 订阅地址（本机 / 局域网 / token）。
#[tauri::command]
fn rss_base() -> store::RssBase {
    return store::rss_base();
}

/// 事件通过 Tauri IPC 兜底回传（首选是 WebView2 原生 postMessage）。
#[tauri::command]
fn browser_event(app: tauri::AppHandle, webview: tauri::Webview, payload: String) -> Result<(), String> {
    browser::ingest(&app, webview.label(), &payload);
    return Ok(());
}

#[tauri::command]
fn browser_events() -> Vec<browser::BrowserEvent> {
    return browser::list_events();
}

#[tauri::command]
fn browser_events_clear() -> usize {
    return browser::clear_events();
}

/// E3：RAG 知识库统计。
#[tauri::command]
fn kb_stats(app: tauri::AppHandle) -> Result<store::KbStats, String> {
    return store::stats(&app);
}

/// E3：在已入库文档里做关键词检索。
#[tauri::command]
fn kb_search(app: tauri::AppHandle, query: String, limit: Option<usize>) -> Result<Vec<store::KbHit>, String> {
    return store::search(&app, &query, limit.unwrap_or(20));
}

/// F：显式丢弃超过 TTL（默认 30 天）的知识库记录。
#[tauri::command]
fn kb_purge(app: tauri::AppHandle) -> Result<usize, String> {
    return store::purge(&app);
}

/// I：主题查询（没搜过立刻搜索入库；命中读 RAG；太旧先给旧数据并后台刷新）。
#[tauri::command]
fn topic_query(app: tauri::AppHandle, query: String, cache_only: Option<bool>) -> Result<store::ServeResult, String> {
    return store::topic_query(&app, &query, cache_only.unwrap_or(false));
}

/// I：任务清单。
#[tauri::command]
fn topic_list(app: tauri::AppHandle) -> Result<Vec<store::Topic>, String> {
    return store::topic_list(&app);
}

/// I：立即刷新一个主题。
#[tauri::command]
fn topic_run(app: tauri::AppHandle, query: String) -> Result<store::TopicRun, String> {
    return store::topic_run(&app, &query);
}

/// I：删除一个主题任务（RAG 文档保留，等 TTL）。
#[tauri::command]
fn topic_remove(app: tauri::AppHandle, query: String) -> Result<bool, String> {
    return store::topic_remove(&app, &query);
}

/// I：把主题时间回拨（运维 / 自检用）。
#[tauri::command]
fn topic_backdate(app: tauri::AppHandle, query: String, secs: u64) -> Result<bool, String> {
    return store::topic_backdate(&app, &query, secs);
}

fn topic_line(t: &store::Topic) -> String {
    let now = c96::store::doc::now_ms();
    let tag = if t.paused { "paused" } else if c96::store::state::is_due(t, now) { "DUE" } else { "wait" };
    return format!("[{}] \"{}\" runs={} stored={} interval={}s fresh={}s next_due_ms={} docs={} status={}",
        tag, t.query, t.runs, t.stored, t.interval_secs, t.fresh_secs, t.next_due_ms, t.docs.len(), t.last_status);
}

fn dump_events(tag: &str) {
    let evs = browser::list_events();
    let mut clicks = 0;
    let mut navs = 0;
    let mut loads = 0;
    let mut ready = 0;
    let mut spa = 0;
    for e in &evs {
        match e.kind.as_str() {
            "click" => clicks += 1,
            "navigation" => navs += 1,
            "load-start" => loads += 1,
            "load-finish" => loads += 1,
            "ready" => ready += 1,
            "spa" => spa += 1,
            _ => {}
        }
    }
    println!(
        "[selftest] events {}: total={} click={} nav={} load={} ready={} spa={}",
        tag, evs.len(), clicks, navs, loads, ready, spa
    );
    for e in &evs {
        println!("  #{:<3} {:<11} {}", e.seq, e.kind, e.url);
    }
}

/// 主窗口启动：先装 web-message 接收器，再导航到起始页。
fn boot_main(app: tauri::AppHandle) {
    std::thread::sleep(std::time::Duration::from_millis(700));
    match browser::install_web_message(&app, "main") {
        Ok(s) => println!("[c96] main web-message: {}", s),
        Err(e) => println!("[c96] main web-message ERR: {}", e),
    }
    match browser::raw_navigate(&app, "main", START_URL) {
        Ok(s) => println!("[c96] main start -> {}", s),
        Err(e) => println!("[c96] main start ERR: {}", e),
    }
    match store::stats(&app) {
        Ok(st) => println!("[c96] RAG 知识库: {} ({} 文档, {} 字节, ttl={}d, purged(open)={})",
            st.path, st.docs, st.bytes, st.ttl_days, st.purged),
        Err(e) => println!("[c96] RAG 知识库 ERR: {}", e),
    }
    match store::topic_list(&app) {
        Ok(ts) => {
            println!("[c96] 主题任务: {} 个（state.toml [[topic]]）", ts.len());
            for t in ts.iter().take(8) { println!("  {}", topic_line(t)); }
        }
        Err(e) => println!("[c96] 主题任务 ERR: {}", e),
    }
    // 常驻调度：程序一直运行 -> 到点的订阅用浏览器逐页爬正文入库（能过反爬）
    spawn_crawl_scheduler(app.clone());
    println!("[c96] 浏览器调度器: 启动（tick={}s，到期订阅自动逐页爬）", c96::research::DEFAULT_SCHED_TICK_SECS);
    // I3/I4：启动即本地 RSS 服务器 —— 关键词搜到新结果就变成 feed 里的新条目（阅读器轮询即见）
    match store::handle(&app) {
        Ok(arc) => {
            let bind = std::env::var("C96_RSS_BIND").unwrap_or_else(|_| c96::rss::DEFAULT_BIND.to_string());
            // 默认不设 token（地址干净，阅读器直接添加最好）；只有显式设 C96_RSS_TOKEN 才要 ?t=
            let token = std::env::var("C96_RSS_TOKEN").unwrap_or_default();
            match c96::rss::spawn_server(arc, &bind, &token) {
                Ok(adv) => {
                    // 面板要能同时给出本机地址与局域网地址：监听通配时 adv 是局域网 IP
                    let local = if c96::rss::is_loopback(&bind) {
                        adv.clone()
                    } else {
                        let port = adv.rsplit(':').next().unwrap_or("").to_string();
                        format!("127.0.0.1:{}", port)
                    };
                    store::set_rss_base(&local, &adv, &token);
                    println!("[c96] RSS 服务已就绪: http://{}/", local);
                    println!("[c96] OPML 一键导入: http://{}/opml.xml", local);
                    if token.is_empty() { println!("[c96] 局域网无 token 直连：阅读器直接添加上面地址；要限制访问就设 C96_RSS_TOKEN"); }
                    else { println!("[c96] 订阅地址须带 ?t={}", token); }
                }
                Err(e) => println!("[c96] RSS 服务未启动: {}", e),
            }
        }
        Err(e) => println!("[c96] RSS 服务 ERR: {}", e),
    }
}

/// E1/E2 无人值守自检（裸 WebView2 + 事件通道）。
fn run_selftest(app: tauri::AppHandle) {
    println!("[selftest] E1+E2 start");
    match browser::open(&app, "https://example.com/") {
        Ok(m) => println!("[selftest] open_browser -> {}", m),
        Err(e) => println!("[selftest] open_browser ERR: {}", e),
    }
    std::thread::sleep(std::time::Duration::from_secs(4));

    if let Some(win) = app.get_webview_window("browser") {
        let js = "document.title = (window.__C96_SNAPSHOT__ ? 'C96_HOOK_OK' : 'C96_HOOK_MISSING');";
        match win.eval(js) {
            Ok(()) => println!("[selftest] eval set title"),
            Err(e) => println!("[selftest] eval ERR: {}", e),
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(400));

    match browser::raw_info(&app, "browser") {
        Ok(m) => println!("[selftest] raw_info(browser) #1:{}", m),
        Err(e) => println!("[selftest] raw_info #1 ERR: {}", e),
    }
    match browser::raw_zoom(&app, "browser", 1.5) {
        Ok(m) => println!("[selftest] raw_zoom -> {}", m),
        Err(e) => println!("[selftest] raw_zoom ERR: {}", e),
    }

    if let Some(win) = app.get_webview_window("browser") {
        let js = "var b=document.createElement('button'); b.id='c96-selftest'; b.textContent='C96 click probe'; document.body.appendChild(b); b.click();";
        match win.eval(js) {
            Ok(()) => println!("[selftest] E2 synthetic click dispatched"),
            Err(e) => println!("[selftest] E2 click ERR: {}", e),
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(900));
    dump_events("after synthetic click");

    match browser::raw_navigate(&app, "browser", "https://www.rust-lang.org/") {
        Ok(m) => println!("[selftest] raw_navigate -> {}", m),
        Err(e) => println!("[selftest] raw_navigate ERR: {}", e),
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    dump_events("after navigate");
    match browser::raw_info(&app, "browser") {
        Ok(m) => println!("[selftest] raw_info(browser) #2:{}", m),
        Err(e) => println!("[selftest] raw_info #2 ERR: {}", e),
    }
    println!("[selftest] E1+E2 done");
    app.exit(0);
}

/// E3 无人值守自检：导航主窗口到一个新 URL，确认正文被自动写入 RAG 并可检索。
fn run_selftest_rag(app: tauri::AppHandle) {
    println!("[selftest-rag] start");
    std::thread::sleep(std::time::Duration::from_millis(2000));
    let before = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    println!("[selftest-rag] kb before = {}", before);
    let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis(),
        Err(_) => 0,
    };
    let url = format!("https://www.rust-lang.org/?c96-e2e={}", stamp);
    match browser::raw_navigate(&app, "main", &url) {
        Ok(m) => println!("[selftest-rag] navigate -> {}", m),
        Err(e) => println!("[selftest-rag] navigate ERR: {}", e),
    }
    // E7：自动入库现在等「正文文本稳定」（最多 6 次 ≈ 6.8s）才送存，所以这里等 9s
    std::thread::sleep(std::time::Duration::from_secs(9));
    let after = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    println!("[selftest-rag] kb after = {} (delta {})", after, after.saturating_sub(before));
    match store::search(&app, "Rust", 5) {
        Ok(h) => {
            println!("[selftest-rag] search 'Rust' hits = {}", h.len());
            for x in h.iter().take(3) {
                println!("  score={} {} {}", x.score, x.host, x.title);
            }
        }
        Err(e) => println!("[selftest-rag] search ERR: {}", e),
    }
    // 去重验证：重新导航同一 URL，应 skip: duplicate url，kb 不增长
    match browser::raw_navigate(&app, "main", &url) {
        Ok(m) => println!("[selftest-rag] re-navigate -> {}", m),
        Err(e) => println!("[selftest-rag] re-navigate ERR: {}", e),
    }
    std::thread::sleep(std::time::Duration::from_secs(9));
    let last_store = browser::list_events().into_iter().rev().find(|e| e.kind == "store" || e.kind == "store-error");
    match last_store {
        Some(e) => println!("[selftest-rag] dup check -> kind={} detail={} url={}", e.kind, e.detail, e.url),
        None => println!("[selftest-rag] dup check -> no store event"),
    }
    let after_dup = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    println!("[selftest-rag] kb after dup = {} (should equal {})", after_dup, after);
    // 搜索输入链路：模拟在叠加条输入并回车 → Bing SERP → 自动入库（带 query）
    if let Some(win) = app.get_webview_window("main") {
        let js = "var i=document.getElementById('c96-q'); if(i){ i.value='c96 端到端检索'; i.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true})); }";
        match win.eval(js) {
            Ok(()) => println!("[selftest-rag] search input simulated"),
            Err(e) => println!("[selftest-rag] search eval ERR: {}", e),
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(7));
    match store::last(&app) {
        Some(d) => println!("[selftest-rag] last stored -> host={} title={} query={} text_len={}", d.host, d.title, d.query, d.text_len),
        None => println!("[selftest-rag] last stored -> none"),
    }
    let evs = browser::list_events();
    println!("[selftest-rag] events = {}", evs.len());
    let tail: Vec<browser::BrowserEvent> = evs.iter().rev().take(10).cloned().collect();
    for e in tail.iter().rev() {
        println!("  #{:<3} {:<11} {}", e.seq, e.kind, e.url);
    }
    // F：TTL 自检 —— 注入 31 天前的记录，purge 必须丢弃（默认 TTL 30 天）
    let before_ttl = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    let probe_text = "过期正文 probe ".to_string() + &"Rust 编译器 存储层 TTL 验证 ".repeat(40);
    match store::inject_backdated(&app, "https://c96-ttl.example.org/probe", "TTL 过期探针", &probe_text, "ttl", 31) {
        Ok(m) => println!("[selftest-rag] ttl inject -> {}", m),
        Err(e) => println!("[selftest-rag] ttl inject ERR: {}", e),
    }
    let mid_ttl = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    match store::purge(&app) {
        Ok(n) => println!("[selftest-rag] ttl purge dropped = {}", n),
        Err(e) => println!("[selftest-rag] ttl purge ERR: {}", e),
    }
    let after_ttl = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    println!("[selftest-rag] kb ttl check: before={} injected={} after_purge={} (expect after == before)",
        before_ttl, mid_ttl, after_ttl);
    // E7：自检自己要清干净 —— 删掉本次导航/搜索产生的记录（rust-lang 带 c96-e2e、Bing SERP 查询是 c96 端到端检索）
    let mut cleaned = 0;
    if let Ok(v) = store::docs_all(&app, "", false, 0) {
        if let Some(rows) = v.get("rows").and_then(|r| r.as_array()) {
            for r in rows.iter() {
                let hash = r.get(0).and_then(|x| x.as_str()).unwrap_or("");
                let u = r.get(1).and_then(|x| x.as_str()).unwrap_or("");
                let q = r.get(5).and_then(|x| x.as_str()).unwrap_or("");
                if (u.contains("c96-e2e") || q == "c96 端到端检索") && store::doc_remove(&app, hash).unwrap_or(false) {
                    cleaned += 1;
                }
            }
        }
    }
    println!("[selftest-rag] 清理自检记录 = {}", cleaned);
    println!("[selftest-rag] done");
    app.exit(0);
}

/// 生成"先算表达式、再用 WebView2 原生通道回传"的探测 JS。
/// 不依赖注入脚本 / __C96_RSS_UI__ 是否成功，纯 DOM + postMessage。
fn post_js(expr: &str) -> String {
    return "(function(){ var m; try { m = String(".to_string() + expr
        + "); } catch (e) { m = 'EXPR-ERR ' + e; } try { window.chrome.webview.postMessage(JSON.stringify({ type: 'rss-ui', url: location.href, detail: m })); } catch (e) {} })();";
}

/// 自检用：直接 GET 本进程 RSS 服务器（端口取 C96_RSS_BIND，默认 127.0.0.1:8787）。
fn rss_fetch(path: &str) -> String {
    use std::io::Read;
    use std::io::Write;
    let bind = std::env::var("C96_RSS_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let addr = if bind.starts_with("0.0.0.0:") { bind.replacen("0.0.0.0:", "127.0.0.1:", 1) } else if bind.contains(':') { bind } else { format!("127.0.0.1:{}", bind) };
    let mut out = String::new();
    if let Ok(mut s) = std::net::TcpStream::connect(&addr) {
        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let req = format!("GET {} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n", path);
        if s.write_all(req.as_bytes()).is_ok() {
            let _ = s.read_to_string(&mut out);
        }
    }
    return out;
}

/// E7 无人值守自检：RSS 内容管理器（注入顶部工具栏的面板）—— 真点击驱动 增 / 改 / 查 / 删。
fn run_selftest_rss_ui(app: tauri::AppHandle) {
    println!("[selftest-rss-ui] start");
    std::thread::sleep(std::time::Duration::from_millis(3000));
    let win = match app.get_webview_window("main") {
        Some(w) => w,
        None => { println!("[selftest-rss-ui] no main window"); app.exit(0); return; }
    };
    let bar = match app.get_webview("toolbar") {
        Some(w) => w,
        None => { println!("[selftest-rss-ui] no toolbar webview"); app.exit(0); return; }
    };
    let ov = match app.get_webview("overlay") {
        Some(w) => w,
        None => { println!("[selftest-rss-ui] no overlay webview"); app.exit(0); return; }
    };
    let _ = &win;
    // 用本地页跑，隔离真站 SPA 的干扰
    let dir = std::env::temp_dir().join("c96_rss_ui");
    let _ = std::fs::create_dir_all(&dir);
    let page = dir.join("page.html");
    let _ = std::fs::write(&page, "<!doctype html><meta charset=utf-8><title>C96 rss ui</title><body><h1>rss ui selftest</h1></body>");
    let page_url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    let _ = browser::raw_navigate(&app, "main", &page_url);
    std::thread::sleep(std::time::Duration::from_millis(2500));

    // 0) 防御性探针：确认注入脚本/图标/IPC 是否就位（不依赖面板自己的上报）
    let _ = bar.eval(&post_js("'probe bar=' + !!document.getElementById('c96-bar') + ' btn=' + !!document.getElementById('c96-rss-btn') + ' ui=' + (typeof window.__C96_RSS_UI__) + ' ipc=' + !!(window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke)"));
    std::thread::sleep(std::time::Duration::from_millis(600));

    // 1) 点顶部工具栏图标 -> 面板打开并加载数据
    let _ = bar.eval("document.getElementById('c96-rss-btn') && document.getElementById('c96-rss-btn').click();");
    std::thread::sleep(std::time::Duration::from_millis(900));
    let _ = ov.eval(&post_js("'afterClick panel=' + !!document.getElementById('c96-rss-panel') + ' display=' + (document.getElementById('c96-rss-panel') ? document.getElementById('c96-rss-panel').style.display : '-')"));
    let _ = ov.eval("window.__C96_RSS_UI__ && window.__C96_RSS_UI__.reload();");
    std::thread::sleep(std::time::Duration::from_millis(3000));
    let _ = ov.eval(&post_js("'reload 状态=' + (document.getElementById('c96-rss-status') ? document.getElementById('c96-rss-status').textContent : '-')"));
    let _ = ov.eval(&post_js("'panel=' + (document.getElementById('c96-rss-panel') ? document.getElementById('c96-rss-panel').style.display : 'missing') + ' 订阅行=' + document.querySelectorAll('#c96-rss-topics > div').length + ' 来源行=' + document.querySelectorAll('#c96-rss-sources > div').length + ' 地址行=' + document.querySelectorAll('#c96-rss-urls > div').length"));
    std::thread::sleep(std::time::Duration::from_millis(700));

    // 1.5) 造 3 条「随手收录」（source=gui-browser，query 空）—— 模拟随便搜索/打开的页面
    let junk_host = "c96-junk.example.com";
    let keep_host = "c96-keep.example.com";
    let _ = store::host_remove(&app, junk_host);
    let _ = store::host_remove(&app, keep_host);
    let _ = store::ignore_host(&app, junk_host, false);
    let mut junk_ok = 0;
    for (u, t, x) in [
        ("https://c96-junk.example.com/a", "C96JUNK-A", "C96 junk alpha body"),
        ("https://c96-junk.example.com/b", "C96JUNK-B", "C96 junk bravo body"),
        ("https://c96-keep.example.com/c", "C96JUNK-C", "C96 junk charlie body"),
    ] {
        if let Ok(m) = store::save(&app, u, t, x, "", 0) {
            if m.starts_with("stored:") { junk_ok += 1; }
        }
    }
    println!("[selftest-rss-ui] 1.5) 造随手收录 {}/3 条（source=gui-browser, query 空）", junk_ok);

    // 2) 增：界面填关键词 + 间隔 -> 点「添加订阅」
    let q = "c96 rss 管理器自检";
    let js_add = "(function(){ document.getElementById('c96-rss-new-q').value = '".to_string()
        + q + "'; document.getElementById('c96-rss-new-int').value = '600'; document.getElementById('c96-rss-add').click(); })();";
    let _ = ov.eval(&js_add);
    std::thread::sleep(std::time::Duration::from_millis(22000));
    let key = c96::store::state::topic_key(q);
    let added = store::topic_list(&app).unwrap_or_default().into_iter().find(|t| t.key == key);
    match &added {
        Some(t) => println!("[selftest-rss-ui] 1) 增 OK: query={} 间隔={}s runs={} stored={} 最近条目={}",
            t.query, t.interval_secs, t.runs, t.stored, t.docs.len()),
        None => println!("[selftest-rss-ui] 1) 增 FAIL: 订阅未建立"),
    }

    // 2.5) 「只看随手收录」：订阅还在时，随手那批留下、订阅那批排除（判定在 Rust 侧）
    let off_shown = store::docs_all(&app, "", false, 300).ok()
        .and_then(|v| v.get("shown").and_then(|x| x.as_u64())).unwrap_or(0);
    let on_docs = store::docs_all(&app, "", true, 300).ok().unwrap_or(serde_json::Value::Null);
    let on_shown = on_docs.get("shown").and_then(|x| x.as_u64()).unwrap_or(0);
    let on_junk = on_docs.get("rows").and_then(|x| x.as_array())
        .map(|a| a.iter().filter(|r| r.get(3).and_then(|h| h.as_str()) == Some(junk_host)).count()).unwrap_or(0);
    let filt_junk = store::docs_all(&app, "C96JUNK", false, 300).ok()
        .and_then(|v| v.get("shown").and_then(|x| x.as_u64())).unwrap_or(0);
    println!("[selftest-rss-ui] 2.5) 只看随手 {}: 全部={} 随手={}（随手里 junk={}）过滤 C96JUNK={}",
        if on_shown < off_shown && on_junk >= 2 { "OK" } else { "FAIL" }, off_shown, on_shown, on_junk, filt_junk);

    // 3) 改：界面改间隔 -> 900
    let _ = ov.eval(&("(function(){ var i = document.getElementById('c96-rss-int-".to_string() + &key
        + "'); var b = document.getElementById('c96-rss-save-" + &key
        + "'); if (i) { i.value = '900'; } if (b) { b.click(); } })();"));
    let probe_el = "'UI 元素 间隔输入=' + (document.getElementById('c96-rss-int-".to_string() + &key
        + "') ? 'yes' : 'no') + ' 保存按钮=' + (document.getElementById('c96-rss-save-" + &key + "') ? 'yes' : 'no')";
    let _ = ov.eval(&post_js(&probe_el));
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let iv = store::topic_list(&app).unwrap_or_default().into_iter().find(|t| t.key == key).map(|t| t.interval_secs).unwrap_or(0);
    println!("[selftest-rss-ui] 2) 改 {}: 界面改间隔后 = {}s（期望 900）", if iv == 900 { "OK" } else { "FAIL" }, iv);

    // 4) 查：界面展开「条目」
    let _ = ov.eval(&("(function(){ var b = document.getElementById('c96-rss-docs-".to_string() + &key + "'); if (b) { b.click(); } })();"));
    std::thread::sleep(std::time::Duration::from_millis(3000));
    let _ = ov.eval(&("(function(){ var box = document.getElementById('c96-rss-items-".to_string() + &key
        + "'); })();"));
    let box_js = "document.getElementById('c96-rss-items-".to_string() + &key + "')";
    let probe_items = "'条目行=' + (".to_string() + &box_js + " ? " + &box_js + ".children.length : -1) + ' 首行=' + (" + &box_js + " ? " + &box_js + ".textContent.slice(0, 70) : '')";
    let _ = ov.eval(&post_js(&probe_items));
    std::thread::sleep(std::time::Duration::from_millis(700));

    // 4a) 全部内容区：真渲染 + 过滤（UI 输入框触发 250ms 防抖 reload）
    let _ = ov.eval("window.__C96_RSS_UI__ && window.__C96_RSS_UI__.reload();");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let _ = ov.eval(&post_js("'全部内容行=' + document.querySelectorAll('#c96-rss-docs > div').length + ' 计数=' + (document.getElementById('c96-rss-dcount') ? document.getElementById('c96-rss-dcount').textContent : '-') + ' 主机行=' + document.querySelectorAll('#c96-rss-hosts > div').length + ' 来源行按钮=' + document.querySelectorAll('#c96-rss-sources > div button').length"));
    std::thread::sleep(std::time::Duration::from_millis(700));
    let _ = ov.eval("(function(){ var el = document.getElementById('c96-rss-dq'); if (el) { el.value = 'C96JUNK-A'; el.dispatchEvent(new Event('input')); } })();");
    std::thread::sleep(std::time::Duration::from_millis(1800));
    let _ = ov.eval(&post_js("'过滤 C96JUNK-A 后 行=' + document.querySelectorAll('#c96-rss-docs > div').length + ' 计数=' + (document.getElementById('c96-rss-dcount') ? document.getElementById('c96-rss-dcount').textContent : '-')"));
    std::thread::sleep(std::time::Duration::from_millis(700));

    // 4b) 逐条删：界面两次点击（第一次变「确认?」）
    let del_row = "(function(){ var row = document.querySelector('#c96-rss-docs > div'); if (!row) { return 'norow'; } var bs = row.querySelectorAll('button'); if (!bs.length) { return 'nobtn'; } bs[bs.length - 1].click(); return 'clicked'; })();";
    // 点击驱动必须是裸 eval（post_js 会把表达式包进 String(...)，带 ; 的 IIFE 会解析失败）
    let _ = ov.eval(del_row);
    std::thread::sleep(std::time::Duration::from_millis(900));
    let _ = ov.eval(del_row);
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let left_a = store::docs_all(&app, "C96JUNK-A", false, 300).ok()
        .and_then(|v| v.get("shown").and_then(|x| x.as_u64())).unwrap_or(1);
    println!("[selftest-rss-ui] 4b) 逐条删（UI） {}: C96JUNK-A 剩余 {} 条", if left_a == 0 { "OK" } else { "FAIL" }, left_a);

    // 4b2) RSS 端到端：删掉的条目必须立刻从 /rss/all.xml 消失（用户诉求就是 RSS 里别再出现）
    let feed = rss_fetch("/rss/all.xml");
    let feed_ok = !feed.contains("C96JUNK-A") && feed.contains("C96JUNK-B");
    println!("[selftest-rss-ui] 4b2) feed 反映删除 {}: all.xml {} 字节 · 含已删的 A={} 含保留的 B={}",
        if feed_ok { "OK" } else { "FAIL" }, feed.len(), feed.contains("C96JUNK-A"), feed.contains("C96JUNK-B"));

    // 4c) 主机制「忽略」：忽略后同站点新页面直接 skip
    let host_btn = |i: usize| -> String {
        return format!("(function(){{ var rows = document.querySelectorAll('#c96-rss-hosts > div'); for (var k = 0; k < rows.length; k++) {{ var nm = rows[k].firstElementChild; if (nm && nm.textContent === '{}') {{ var bs = rows[k].querySelectorAll('button'); if (bs.length <= {}) {{ return 'nobtn'; }} bs[{}].click(); return 'clicked'; }} }} return 'no-row'; }})();", junk_host, i, i);
    };
    let _ = ov.eval(&host_btn(1));
    std::thread::sleep(std::time::Duration::from_millis(1800));
    let ig_on = store::ignored_hosts(&app).unwrap_or_default().iter().any(|h| h == junk_host);
    let skip_msg = store::save(&app, "https://c96-junk.example.com/skip-probe", "C96JUNK-SKIP", "ignored host probe body", "", 0).unwrap_or_default();
    println!("[selftest-rss-ui] 4c) 忽略站点 {}: 名单命中={} 新页面={}", if ig_on && skip_msg.starts_with("skip: ignored host") { "OK" } else { "FAIL" }, ig_on, skip_msg);

    // 4d) 取消忽略：应当恢复收录
    let _ = ov.eval(&host_btn(1));
    std::thread::sleep(std::time::Duration::from_millis(1800));
    let ig_off = !store::ignored_hosts(&app).unwrap_or_default().iter().any(|h| h == junk_host);
    let again_msg = store::save(&app, "https://c96-junk.example.com/skip-probe", "C96JUNK-SKIP", "ignored host probe body", "", 0).unwrap_or_default();
    println!("[selftest-rss-ui] 4d) 取消忽略 {}: 名单已空={} 再存={}", if ig_off && again_msg.starts_with("stored:") { "OK" } else { "FAIL" }, ig_off, again_msg);

    // 4e) 主机制「清空」：两次点击 -> 该站点内容归零
    let keep_btn = format!("(function(){{ var rows = document.querySelectorAll('#c96-rss-hosts > div'); for (var k = 0; k < rows.length; k++) {{ var nm = rows[k].firstElementChild; if (nm && nm.textContent === '{}') {{ var bs = rows[k].querySelectorAll('button'); if (!bs.length) {{ return 'nobtn'; }} bs[0].click(); return 'clicked'; }} }} return 'no-row'; }})();", keep_host);
    let _ = ov.eval(&keep_btn);
    std::thread::sleep(std::time::Duration::from_millis(900));
    let _ = ov.eval(&keep_btn);
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let kept_left = store::docs_all(&app, keep_host, false, 300).ok()
        .and_then(|v| v.get("shown").and_then(|x| x.as_u64())).unwrap_or(1);
    println!("[selftest-rss-ui] 4e) 清空主机 {}: 剩余 {} 条", if kept_left == 0 { "OK" } else { "FAIL" }, kept_left);

    // 5) 删：界面两次点击（第一次变「确认删除?」）
    let del_js = "(function(){ var b = document.getElementById('c96-rss-del-".to_string() + &key + "'); if (b) { b.click(); } })();";
    let _ = ov.eval(&del_js);
    std::thread::sleep(std::time::Duration::from_millis(900));
    let _ = ov.eval(&del_js);
    std::thread::sleep(std::time::Duration::from_millis(2600));
    let gone = !store::topic_list(&app).unwrap_or_default().iter().any(|t| t.key == key);
    println!("[selftest-rss-ui] 3) 删 {}: 订阅已{}", if gone { "OK" } else { "FAIL" }, if gone { "移除" } else { "仍存在" });

    // 6) 真站回归：外部 https 页面（Bing）上图标/面板同样可用
    let _ = browser::raw_navigate(&app, "main", "https://www.bing.com/");
    std::thread::sleep(std::time::Duration::from_secs(7));
    let _ = bar.eval(&post_js("'bing bar=' + !!document.getElementById('c96-bar') + ' btn=' + !!document.getElementById('c96-rss-btn')"));
    let _ = bar.eval("document.getElementById('c96-rss-btn') && document.getElementById('c96-rss-btn').click();");
    std::thread::sleep(std::time::Duration::from_millis(4000));
    let _ = ov.eval(&post_js("'bing panel=' + (document.getElementById('c96-rss-panel') ? document.getElementById('c96-rss-panel').style.display : 'missing') + ' 订阅行=' + document.querySelectorAll('#c96-rss-topics > div').length + ' 状态=' + (document.getElementById('c96-rss-status') ? document.getElementById('c96-rss-status').textContent : '-')"));
    std::thread::sleep(std::time::Duration::from_millis(700));

    // 7) 面板上报 + 清理自检条目（顺便验证「删条目」）
    for e in browser::list_events().iter().filter(|e| e.kind == "rss-ui" || e.kind == "rss-op") {
        println!("[selftest-rss-ui] ui> {}", e.detail);
    }
    let mut removed = 0;
    for r in store::topic_docs(&app, q, 50).unwrap_or_default().iter() {
        if store::doc_remove(&app, &r.0).unwrap_or(false) { removed += 1; }
    }
    println!("[selftest-rss-ui] 清理自检条目 = {}（KB 现有 {} 条）", removed,
        store::stats(&app).map(|s| s.docs).unwrap_or(0));
    let junk_left = store::host_remove(&app, junk_host).unwrap_or(0) + store::host_remove(&app, keep_host).unwrap_or(0);
    let _ = store::ignore_host(&app, junk_host, false);
    let _ = store::ignore_host(&app, keep_host, false);
    println!("[selftest-rss-ui] 清理随手收录 = {} 条 · 忽略名单 = {:?} · KB 现有 {} 条",
        junk_left, store::ignored_hosts(&app).unwrap_or_default(), store::stats(&app).map(|s| s.docs).unwrap_or(0));
    println!("[selftest-rss-ui] done");
    app.exit(0);
}

fn tail_url(u: &str) -> String {
    let s = u.rsplit('/').next().unwrap_or(u);
    let s = if s.is_empty() { u } else { s };
    return s.chars().take(90).collect();
}

/// 链接点击自检：用户反馈"点搜索结果没反应"。
/// 本地页面里放三种链接（普通 / target=_blank / window.open），逐一点击看窗口 URL 有没有变。
fn run_selftest_links(app: tauri::AppHandle) {
    println!("[selftest-links] start");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let dir = std::env::temp_dir().join("c96_links");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("target.html"),
        "<!doctype html><meta charset=utf-8><title>C96 target</title><body><h1>target page</h1></body>");
    let _ = std::fs::write(dir.join("links.html"),
        "<!doctype html><meta charset=utf-8><title>C96 links</title><body>\
         <a id=\"plain\" href=\"target.html\">plain</a>\
         <a id=\"blank\" href=\"target.html?kind=blank\" target=\"_blank\">blank</a>\
         <button id=\"pop\" onclick=\"window.open('target.html?kind=pop','_blank')\">pop</button>\
         </body>");
    let page_url = format!("file:///{}", dir.join("links.html").to_string_lossy().replace('\\', "/"));
    let win = match app.get_webview_window("main") {
        Some(w) => w,
        None => { println!("[selftest-links] 主窗口不存在"); app.exit(0); return; }
    };
    let cases: [(&str, &str); 3] = [
        ("plain", "document.getElementById('plain').click();"),
        ("blank", "document.getElementById('blank').click();"),
        ("pop", "document.getElementById('pop').click();"),
    ];
    let mut pass = 0;
    for (name, js) in cases {
        match browser::raw_navigate(&app, "main", &page_url) {
            Ok(_) => {}
            Err(e) => println!("[selftest-links] navigate ERR: {}", e),
        }
        std::thread::sleep(std::time::Duration::from_millis(1600));
        let before = win.url().map(|u| u.to_string()).unwrap_or_default();
        let loaded = before.contains("links.html");
        let _ = win.eval(js);
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let after = win.url().map(|u| u.to_string()).unwrap_or_default();
        let ok = loaded && after.contains("target.html");
        if ok { pass += 1; }
        println!("[selftest-links] {:<5} 页面就绪={} {} -> {} => {}",
            name, loaded, tail_url(&before), tail_url(&after), if ok { "OK 打开了" } else { "FAIL 没反应" });
    }
    // 真站：Bing 结果页第一枚结果
    match browser::raw_navigate(&app, "main", "https://www.bing.com/search?q=%E5%8D%A1%E6%99%AE%E7%A9%BA") {
        Ok(_) => {}
        Err(e) => println!("[selftest-links] bing navigate ERR: {}", e),
    }
    std::thread::sleep(std::time::Duration::from_secs(7));
    let before = win.url().map(|u| u.to_string()).unwrap_or_default();
    let _ = win.eval("(function(){var a=document.querySelectorAll('#b_results h2 a')[0]; if(a){a.click();}})();");
    std::thread::sleep(std::time::Duration::from_secs(6));
    let after = win.url().map(|u| u.to_string()).unwrap_or_default();
    let bing_ok = before.contains("bing.com") && !after.contains("bing.com");
    println!("[selftest-links] bing  {} -> {} => {}", tail_url(&before), tail_url(&after),
        if bing_ok { "OK 结果页点开了" } else { "FAIL 结果页点击无反应" });
    println!("[selftest-links] 小结: 本地 3 例通过 {}/3，Bing 结果页 {}", pass, if bing_ok { "通过" } else { "失败" });
    println!("[selftest-links] done");
    app.exit(0);
}

/// E7 无人值守自检：自动入库必须走「正文抽取」，不能把菜单/侧栏/推荐整页塞进知识库。
///   1) 主流站布局的本地页（顶部菜单 + 正文 5 段 + 侧栏相关 + 评论区 + 页脚）
///      → 自动入库后：正文 5 段都在，菜单/侧栏/相关/评论/页脚词一个都不在，mode=cluster
///   2) 纯链接页（40 个导航链接）→ 被链接密度闸门跳过（KB 不增长），留下 store-skip 事件
fn run_selftest_extract(app: tauri::AppHandle) {
    println!("[selftest-extract] start");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let dir = std::env::temp_dir().join("c96_extract");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis(),
        Err(_) => 0,
    };
    let before = store::stats(&app).map(|s| s.docs).unwrap_or(0);

    // ---- 页面 1：文章页（菜单 + 正文 + 侧栏 + 评论 + 页脚）----
    let mut page = String::new();
    page.push_str("<!doctype html><meta charset=utf-8><title>C96EXTRACT 主流站布局</title><body>");
    page.push_str("<div class=\"top-nav\"><a href=\"#\">新闻</a><a href=\"#\">体育</a><a href=\"#\">汽车</a><a href=\"#\">房产</a><a href=\"#\">旅游</a><a href=\"#\">教育</a><a href=\"#\">时尚</a><a href=\"#\">登录</a></div>");
    page.push_str("<div class=\"crumb\">首页 &gt; 科技 &gt; 正文</div>");
    page.push_str("<div class=\"article-content\"><h1>C96EXTRACT-TITLE 正文标题</h1><div class=\"meta\">2026-10-12 · 来源：C96 自检</div>");
    for i in 1..=6 {
        page.push_str(&format!("<p>C96EXTRACT-P{} 这是正文第 {} 段：主流网站的正文往往夹在导航条、侧栏、相关推荐与评论区之间，自动入库必须只摘正文块；这一段如果没有出现在知识库里，就说明正文抽取退化成了整页 innerText。</p>", i, i));
    }
    page.push_str("<p>参考链接：<a href=\"https://example.com/ref\">Rust 官网</a> 之后继续正文。</p></div>");
    page.push_str("<aside class=\"sidebar\"><div class=\"related-news\"><a href=\"#\">C96EXTRACT-REL 相关推荐一</a><a href=\"#\">C96EXTRACT-REL 相关推荐二</a></div></aside>");
    page.push_str("<div class=\"comment-area\"><p>C96EXTRACT-COMMENT 评论区内容不该入库</p></div>");
    page.push_str("<div class=\"footer-bar\"><a href=\"#\">关于我们</a><a href=\"#\">广告投放</a><a href=\"#\">联系方式</a></div>");
    page.push_str("</body>");
    let art = dir.join("article.html");
    let _ = std::fs::write(&art, &page);
    let art_url = format!("file:///{}?c96={}", art.to_string_lossy().replace('\\', "/"), stamp);
    let _ = browser::raw_navigate(&app, "main", &art_url);
    // 抽取要等文本稳定（首读 1s + 稳定判定），短页面走 6 次兜底 ≈ 7s
    std::thread::sleep(std::time::Duration::from_secs(9));
    let after = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    let doc = store::last(&app);
    let text = doc.as_ref().map(|d| d.text.clone()).unwrap_or_default();
    let doc_url_ok = doc.as_ref().map(|d| d.url.contains("article.html")).unwrap_or(false);
    let mut p_hits = 0;
    for i in 1..=6 {
        if text.contains(&format!("C96EXTRACT-P{}", i)) { p_hits += 1; }
    }
    let mut junk: Vec<&str> = Vec::new();
    for w in ["体育", "旅游", "登录", "C96EXTRACT-REL", "C96EXTRACT-COMMENT", "广告投放", "关于我们"] {
        if text.contains(w) { junk.push(w); }
    }
    let mode = doc.as_ref().map(|d| d.mode.clone()).unwrap_or_default();
    let mode_ok = mode == "cluster" || mode == "semantic" || mode == "article";
    let chars = text.chars().count();
    let one_ok = doc_url_ok && p_hits == 6 && junk.is_empty() && mode_ok && chars >= 300 && after > before;
    println!("[selftest-extract] 1) 文章页 {}: 入库 {} 条 · 正文段落 {}/6 · {} 字 · mode={} · 混入词={:?}",
        if one_ok { "OK" } else { "FAIL" }, after.saturating_sub(before), p_hits, chars, mode, junk);
    if let Some(d) = doc.as_ref() {
        let head: String = d.text.chars().take(90).collect();
        println!("[selftest-extract]    入库开头: {}", head.replace(char::from(10), " / "));
    }
    let load_events: Vec<String> = browser::list_events().into_iter()
        .filter(|e| e.url.contains("article.html") && (e.kind == "ready" || e.kind == "load"))
        .map(|e| e.kind.clone())
        .collect();
    println!("[selftest-extract]    页面事件: {:?}", load_events);
    let last_store = browser::list_events().into_iter().rev().find(|e| e.kind == "store" && e.url.contains("article.html"));
    match last_store {
        Some(e) => println!("[selftest-extract]    store 事件: {}", e.detail),
        None => println!("[selftest-extract]    store 事件: 无"),
    }

    // ---- 页面 2：纯链接页（导航堆）→ 链接密度闸门应拦住 ----
    let mut links = String::new();
    for i in 0..40 {
        links.push_str(&format!("<a href=\"https://example.com/l{}\">C96EXTRACT-LINK-{} 一条纯导航链接文本</a>", i, i));
    }
    let list_page = format!("<!doctype html><meta charset=utf-8><title>C96EXTRACT 链接堆</title><body><div class=\"listing\">{}</div></body>", links);
    let lp = dir.join("links.html");
    let _ = std::fs::write(&lp, &list_page);
    let lp_url = format!("file:///{}?c96={}", lp.to_string_lossy().replace('\\', "/"), stamp + 1);
    let before2 = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    let _ = browser::raw_navigate(&app, "main", &lp_url);
    std::thread::sleep(std::time::Duration::from_secs(7));
    let after2 = store::stats(&app).map(|s| s.docs).unwrap_or(0);
    let skips: Vec<String> = browser::list_events().into_iter()
        .filter(|e| e.kind == "store-skip" && e.url.contains("links.html"))
        .map(|e| e.detail.clone())
        .collect();
    let two_ok = after2 == before2 && !skips.is_empty();
    println!("[selftest-extract] 2) 链接堆页 {}: KB 增长 {} 条（期望 0）· store-skip {} 个 {:?}",
        if two_ok { "OK" } else { "FAIL" }, after2.saturating_sub(before2), skips.len(), skips.first());

    // ---- 清理：把自检入库的那条删掉，别污染真实知识库 ----
    // 只删自检自己造的这条（防止误删用户知识库里最后一条）
    let removed = match doc.as_ref() {
        Some(d) => {
            if d.url.contains("article.html") { store::doc_remove(&app, &d.hash).unwrap_or(false) } else { false }
        }
        None => false,
    };
    let _ = std::fs::remove_file(&art);
    let _ = std::fs::remove_file(&lp);
    println!("[selftest-extract] 清理自检条目 = {}（KB 现有 {} 条）", removed, store::stats(&app).map(|s| s.docs).unwrap_or(0));
    // ---- 3)（可选）真站重访：C96_EXTRACT_LIVE=url1,url2
    //        专门验证「旧的整页 raw 记录会被一次干净重访替换」（用户实际抱怨的那两个页面就是这么修的）
    let live = std::env::var("C96_EXTRACT_LIVE").unwrap_or_default();
    let live_list: Vec<&str> = live.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if !live_list.is_empty() {
        println!("[selftest-extract] 3) 真站重访 {} 个 URL（抽取后应替换旧的整页记录）", live_list.len());
        for u in live_list.iter() {
            let _ = browser::raw_navigate(&app, "main", u);
            std::thread::sleep(std::time::Duration::from_secs(13));
            let recent: Vec<String> = browser::list_events().into_iter()
                .filter(|e| e.kind == "store" || e.kind == "store-skip" || e.kind == "store-error")
                .rev().take(3)
                .map(|e| format!("{} [{}] {}", e.kind, e.url.chars().take(64).collect::<String>(), e.detail))
                .collect();
            println!("[selftest-extract]    {}", u);
            for r in recent.iter().rev() { println!("[selftest-extract]      事件 {}", r); }
            if let Ok(v) = store::docs_all(&app, "", false, 4) {
                if let Some(rows) = v.get("rows").and_then(|r| r.as_array()) {
                    for r in rows.iter() {
                        let host = r.get(3).and_then(|x| x.as_str()).unwrap_or("");
                        let len = r.get(7).and_then(|x| x.as_u64()).unwrap_or(0);
                        let url = r.get(1).and_then(|x| x.as_str()).unwrap_or("");
                        println!("[selftest-extract]      KB: {} {} 字 {}", host, len, url.chars().take(58).collect::<String>());
                    }
                }
            }
        }
    }

    println!("[selftest-extract] 小结: 文章页 {} · 链接堆页 {}", if one_ok { "通过" } else { "失败" }, if two_ok { "通过" } else { "失败" });
    println!("[selftest-extract] done");
    app.exit(0);
}

fn topic_runs(app: &tauri::AppHandle, q: &str) -> u64 {
    let key = c96::store::state::topic_key(q);
    match store::topic_list(app) {
        Ok(v) => v.into_iter().find(|t| t.key == key).map(|t| t.runs).unwrap_or(0),
        Err(_) => 0,
    }
}

/// I 无人值守自检：
///   1) 首次查询 -> 立刻搜索 + 建任务（state=new）
///   2) 再查询   -> 命中 RAG，不联网（runs 不增加、耗时可忽略）
///   3) 回拨 7h  -> stale：先返回旧数据，同时后台刷新（runs 增加）
///   4) 任务清单持久化（state.toml 的 [[topic]]）
fn run_selftest_topics(app: tauri::AppHandle) {
    println!("[selftest-topics] start");
    std::thread::sleep(std::time::Duration::from_millis(2000));
    let q = "c96 topic selftest 卡普空";
    match store::topic_query(&app, q, false) {
        Ok(r) => println!("[selftest-topics] 1) 首搜: state={} from_cache={} hits={} run={} topic_runs={} stored={}",
            r.state, r.from_cache, r.hits.len(), r.run.is_some(), r.topic.runs, r.topic.stored),
        Err(e) => println!("[selftest-topics] 1) ERR: {}", e),
    }
    let runs1 = topic_runs(&app, q);
    let t0 = std::time::Instant::now();
    match store::topic_query(&app, q, false) {
        Ok(r) => println!("[selftest-topics] 2) 命中: state={} from_cache={} hits={} run={} elapsed={}ms",
            r.state, r.from_cache, r.hits.len(), r.run.is_some(), t0.elapsed().as_millis()),
        Err(e) => println!("[selftest-topics] 2) ERR: {}", e),
    }
    let runs2 = topic_runs(&app, q);
    println!("[selftest-topics] 2b) runs {} -> {}（命中不得增加；耗时 {}ms）", runs1, runs2, t0.elapsed().as_millis());
    match store::topic_backdate(&app, q, 7 * 3600) {
        Ok(b) => println!("[selftest-topics] 3) 回拨 7h -> {}", b),
        Err(e) => println!("[selftest-topics] 3) ERR: {}", e),
    }
    match store::topic_query(&app, q, false) {
        Ok(r) => println!("[selftest-topics] 3b) 太旧: state={} from_cache={} hits={} pending={:?}",
            r.state, r.from_cache, r.hits.len(), r.pending),
        Err(e) => println!("[selftest-topics] 3b) ERR: {}", e),
    }
    let mut waited: u64 = 0;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        waited += 1;
        let now_runs = topic_runs(&app, q);
        if now_runs > runs2 {
            println!("[selftest-topics] 4) 后台刷新完成: runs {} -> {}（等待 {}s）", runs2, now_runs, waited);
            break;
        }
        if waited >= 45 {
            println!("[selftest-topics] 4) 后台刷新超时（runs 仍为 {}）", now_runs);
            break;
        }
    }
    match store::topic_list(&app) {
        Ok(ts) => {
            println!("[selftest-topics] 5) 任务清单: {} 个", ts.len());
            for t in ts.iter().take(6) { println!("  {}", topic_line(t)); }
        }
        Err(e) => println!("[selftest-topics] 5) ERR: {}", e),
    }
    println!("[selftest-topics] done");
    app.exit(0);
}

/// 临时验证：窗口在屏内时，navigate 一个长正文页 + 轮询自动入库事件；
/// 若无入库则 eval 探测页面状态，定位注入链是否在真实页面触发。
fn run_selftest_crawl(app: tauri::AppHandle) {
    let url = "https://zh.wikipedia.org/wiki/%E6%81%90%E6%80%96%E7%94%B5%E5%BD%B1";
    browser::set_query("selftest-crawl");
    let vis = app.get_webview_window("main").map(|w| w.is_visible().unwrap_or(false));
    println!("[selftest-crawl] window visible={:?}", vis);
    match browser::raw_navigate(&app, "main", url) {
        Ok(m) => println!("[selftest-crawl] navigate -> {}", m),
        Err(e) => println!("[selftest-crawl] navigate ERR: {}", e),
    }
    let t0 = std::time::Instant::now();
    let mut found: Option<browser::BrowserEvent> = None;
    while t0.elapsed() < std::time::Duration::from_secs(40) {
        std::thread::sleep(std::time::Duration::from_millis(500));
        for e in browser::list_events().iter().rev() {
            if (e.kind == "store" || e.kind == "store-skip" || e.kind == "store-error")
                && e.url.contains("wikipedia.org") {
                found = Some(e.clone());
                break;
            }
        }
        if found.is_some() { break; }
    }
    match found {
        Some(e) => println!("[selftest-crawl] ON-SCREEN RENDER OK: kind={} url={} detail={}", e.kind, e.url, e.detail),
        None => {
            let probe = browser::eval_string(&app, "main", "(function(){return JSON.stringify({href:location.href,ready:document.readyState,bar:!!document.getElementById('c96-bar'),ex:!!window.__C96_EXTRACT__});})();");
            println!("[selftest-crawl] NO store in 40s. page probe={:?}", probe);
            let evs = browser::list_events();
            for e in evs.iter().rev().take(12) { println!("   ev {} {} {}", e.kind, e.url, e.detail); }
        }
    }
    app.exit(0);
}

/// 浏览器版调度器：到点的订阅，串行把候选导航进主窗口逐页抽正文入库（能过反爬）。
fn spawn_crawl_scheduler(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let tick = c96::research::DEFAULT_SCHED_TICK_SECS;
        loop {
            if let Ok(arc) = store::handle(&app) {
                let keys = {
                    let g = arc.lock().unwrap_or_else(|e| e.into_inner());
                    c96::research::due_keys(&g, c96::store::doc::now_ms())
                };
                for k in keys {
                    let urls = {
                        let g = arc.lock().unwrap_or_else(|e| e.into_inner());
                        c96::research::candidate_urls(&g, &k, &store::run_opts())
                    };
                    if let Ok(u) = urls {
                        if u.is_empty() { continue; }
                        let rep = browser::crawl(&app, &u, &k);
                        let mut g = arc.lock().unwrap_or_else(|e| e.into_inner());
                        c96::research::finalize_crawl(&mut g, &k, rep.stored, c96::store::doc::now_ms());
                        println!("[c96 crawl] \"{}\": {} 页 · 入库 {} · 跳过 {} · 失败 {}",
                            k, u.len(), rep.stored, rep.skipped, rep.failed);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(tick));
        }
    });
}

/// 在参数里找 `--query <值>`；值含空格时会被拆成多个 token，这里把到下一个 `--flag`
/// 之前的所有 token 用空格拼回完整值。
fn arg_after(args: &[String], key: &str) -> Option<String> {
    for i in 0..args.len() {
        if args[i] == key {
            let mut parts: Vec<String> = Vec::new();
            let mut j = i + 1;
            while j < args.len() && !args[j].starts_with("--") {
                parts.push(args[j].clone());
                j += 1;
            }
            if parts.is_empty() { return None; }
            return Some(parts.join(" "));
        }
    }
    return None;
}

fn main() {
    let selftest = std::env::args().any(|a| a == "--selftest");
    let selftest_topics = std::env::args().any(|a| a == "--selftest-topics");
    let selftest_rag = std::env::args().any(|a| a == "--selftest-rag");
    let selftest_links = std::env::args().any(|a| a == "--selftest-links");
    let selftest_rss_ui = std::env::args().any(|a| a == "--selftest-rss-ui");
    let selftest_extract = std::env::args().any(|a| a == "--selftest-extract");
    let selftest_crawl = std::env::args().any(|a| a == "--selftest-crawl");
    // 静默模式（C96 gui --silent）：主窗口创建即隐藏，后台 RSS / 调度照常跑；
    // 自检需要窗口交互，强制可见。
    let silent = std::env::args().any(|a| a == "--silent" || a == "--hidden");
    let any_selftest = selftest || selftest_topics || selftest_rag
        || selftest_links || selftest_rss_ui || selftest_extract;
    // 有 --query（要浏览器爬取）时窗口必须可见才能渲染；静默爬取用小窗。
    let start_query: Option<String> = {
        let a: Vec<String> = std::env::args().collect();
        arg_after(&a, "--query")
    };
    let visible = any_selftest || !silent || start_query.is_some();
    let small = silent && visible;
    tauri::Builder::default()
        // 单实例：再次 `C96 gui` 不再开新进程，而是转发参数给已在跑的实例。
        // 非静默调用 → 唤起已有窗口；带 --silent 的调用（AI 后台调取）→ 不唤窗。
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // --query 转发：让已在跑的实例用浏览器逐页爬这个订阅（小窗，不抢焦点）。
            if let Some(q) = arg_after(&args, "--query") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                }
                let a = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    let _ = store::crawl_topic(&a, &q);
                });
                return;
            }
            if args.iter().any(|a| a == "--silent" || a == "--hidden") {
                return;
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .invoke_handler(tauri::generate_handler![
            banner,
            open_browser,
            snapshot_probe,
            webview2_info,
            webview2_zoom,
            webview2_navigate,
            browser_event,
            browser_events,
            browser_events_clear,
            kb_stats,
            kb_search,
            kb_purge,
            topic_query,
            topic_list,
            topic_run,
            topic_remove,
            topic_backdate,
            topic_update,
            topic_docs,
            rss_sources,
            rss_doc_remove,
            rss_base
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            // 默认（含 --selftest）：主窗口 = 对外浏览器，先 about:blank，装好接收器再导航。
            if let Err(e) = browser::open_main(&handle, "about:blank", visible, small) {
                println!("[c96] open_main ERR: {}", e);
            }
            let boot = handle.clone();
            std::thread::spawn(move || boot_main(boot));
            // 启动自带 --query：窗口就绪后自动用浏览器逐页爬这个订阅。
            if let Some(q) = start_query.clone() {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1400));
                    if let Ok(p) = std::env::var("C96_CRAWL_LOG") {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                            let _ = writeln!(f, "[setup] start_query parsed: {}", q);
                        }
                    }
                    match store::crawl_topic(&h, &q) {
                        Ok(m) => println!("[c96 crawl] {}", m),
                        Err(e) => println!("[c96 crawl] ERR: {}", e),
                    }
                });
            }
            if selftest {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest(h));
            }
            if selftest_rag {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_rag(h));
            }
            if selftest_topics {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_topics(h));
            }
            if selftest_links {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_links(h));
            }
            if selftest_extract {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_extract(h));
            }
            if selftest_rss_ui {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_rss_ui(h));
            }
            if selftest_crawl {
                let h = handle.clone();
                std::thread::spawn(move || run_selftest_crawl(h));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running C96 GUI");
}
