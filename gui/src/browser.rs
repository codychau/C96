//! E1/E2：自控窗口（self-controlled browsing surface）。
//!
//! Tauri v2 在 Windows 上默认 WebView2。本模块负责：
//!   1. 打开 label=browser 的第二个 WebView2 窗口，加载用户给出的 URL
//!   2. initialization_script 注入快照钩子 + 事件钩子（等价 WebView2
//!      AddScriptToExecuteOnDocumentCreated；E3 的 JS 抽取入口）
//!   3. on_navigation / on_page_load 记录用户点击/跳转/加载
//!   4. 通过 Webview::with_webview 拿 PlatformWebview → webview2-com
//!      ICoreWebView2Controller / ICoreWebView2，裸读状态、缩放、导航
//!   5. E2：AddWebMessageReceived 接收页面里 window.chrome.webview.postMessage
//!      发回的点击/SPA 事件，写进事件日志并 emit browser://event
//!
//! 事件走 WebView2 原生 postMessage（不依赖 Tauri IPC 的远端 origin 策略），
//! 便于 E3 回传「终态 DOM 抽取」的大结构化体。

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;

use tauri::AppHandle;
use tauri::Emitter;
use tauri::LogicalPosition;
use tauri::LogicalSize;
use tauri::Manager;
use tauri::PhysicalPosition;
use tauri::PhysicalSize;
use tauri::WebviewUrl;
use tauri::WebviewWindowBuilder;
use tauri::WindowEvent;

/// 所有 WebView2 窗口统一的 Chromium 参数：保留 wry 默认项，并永久静音，
/// 避免网页自动播放音乐/视频产生噪声（`--mute-audio`）。
const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --mute-audio";

/// 事件记录（E2）：点击 / 跳转 / 加载 / SPA / 页面主动上报。
#[derive(Clone, serde::Serialize)]
pub struct BrowserEvent {
    pub seq: u64,
    pub kind: String,
    pub url: String,
    pub detail: String,
    pub at_ms: u128,
}

static EVENTS: OnceLock<Mutex<Vec<BrowserEvent>>> = OnceLock::new();

fn events() -> &'static Mutex<Vec<BrowserEvent>> {
    return EVENTS.get_or_init(|| Mutex::new(Vec::new()));
}

static CURRENT_QUERY: OnceLock<Mutex<String>> = OnceLock::new();

fn query_cell() -> &'static Mutex<String> {
    return CURRENT_QUERY.get_or_init(|| Mutex::new(String::new()));
}

pub fn set_query(q: &str) {
    let mut g = query_cell().lock().unwrap_or_else(|e| e.into_inner());
    *g = q.to_string();
}

pub fn current_query() -> String {
    let g = query_cell().lock().unwrap_or_else(|e| e.into_inner());
    return g.clone();
}

/// 覆盖层当前模式：0 隐藏 / 1 RSS 面板 / 2 设置 / 3 遮罩。
static UI_MODE: AtomicU64 = AtomicU64::new(0);

fn overlay_show(app: &AppHandle) -> Result<tauri::Webview, String> {
    let wv = app.get_webview("overlay").ok_or_else(|| "no overlay webview".to_string())?;
    let _ = wv.show();
    return Ok(wv);
}

pub fn ui_hide(app: &AppHandle) -> Result<String, String> {
    let _ = eval_string(app, "overlay", "window.__C96_UI__&&window.__C96_UI__.hide();");
    if let Some(wv) = app.get_webview("overlay") {
        let _ = wv.hide();
    }
    UI_MODE.store(0, Ordering::Relaxed);
    return Ok("ui hidden".to_string());
}

pub fn ui_command(app: &AppHandle, op: &str) -> Result<String, String> {
    if op == "close" || op == "hide" {
        return ui_hide(app);
    }
    if op == "panel" {
        if UI_MODE.load(Ordering::Relaxed) == 1 {
            return ui_hide(app);
        }
        overlay_show(app)?;
        UI_MODE.store(1, Ordering::Relaxed);
        let _ = eval_string(app, "overlay", "window.__C96_UI__&&window.__C96_UI__.panel();");
        return Ok("panel".to_string());
    }
    if op == "settings" {
        if UI_MODE.load(Ordering::Relaxed) == 2 {
            return ui_hide(app);
        }
        overlay_show(app)?;
        UI_MODE.store(2, Ordering::Relaxed);
        let _ = eval_string(app, "overlay", "window.__C96_UI__&&window.__C96_UI__.settings();");
        return Ok("settings".to_string());
    }
    return Ok("unknown ui op".to_string());
}

/// 浏览器爬取期间的稳定遮罩（独立覆盖层 WebView，不随目标页刷新）。
pub fn ui_mask(app: &AppHandle, text: &str) {
    if overlay_show(app).is_err() {
        return;
    }
    UI_MODE.store(3, Ordering::Relaxed);
    let lit = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let _ = eval_string(app, "overlay", &format!("window.__C96_UI__&&window.__C96_UI__.mask({});", lit));
}

pub fn ui_mask_hide(app: &AppHandle) {
    if UI_MODE.load(Ordering::Relaxed) == 3 {
        let _ = ui_hide(app);
    }
}

fn now_ms() -> u128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => return d.as_millis(),
        Err(_) => return 0,
    }
}

/// 追加一条事件到日志，并广播给前端（browser://event）。
pub fn record(app: &AppHandle, kind: &str, url: &str, detail: &str) {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let ev = BrowserEvent {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        kind: kind.to_string(),
        url: url.to_string(),
        detail: detail.to_string(),
        at_ms: now_ms(),
    };
    {
        let mut v = events().lock().unwrap_or_else(|e| e.into_inner());
        v.push(ev.clone());
        let len = v.len();
        if len > 500 {
            v.drain(0..len - 500);
        }
    }
    let _ = app.emit("browser://event", &ev);
}

/// 生成把结果回送页面的脚本：window.__C96_RSS_UI__.reply(id, ok, json)
fn rss_reply_js(id: &str, ok: bool, payload: &str) -> String {
    let id_lit = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".to_string());
    let data_lit = serde_json::to_string(payload).unwrap_or_else(|_| "\"\"".to_string());
    return "(function(){ var t = window.__C96_RSS_UI__; if (t && t.reply) { t.reply(".to_string()
        + &id_lit + ", " + (if ok { "true" } else { "false" }) + ", " + &data_lit + "); } })();";
}

/// 解析页面 postMessage 的 JSON 负载（`label` 是发送方 WebView，用于回送回复）。
pub fn ingest(app: &AppHandle, label: &str, raw: &str) {
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            record(app, "message", "", raw);
            return;
        }
    };
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("message");
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
    let detail = v.get("detail").and_then(|x| x.as_str()).unwrap_or("");
    if kind == "search" {
        set_query(detail);
        let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("");
        if !target.is_empty() {
            let _ = raw_navigate(app, "main", target);
        }
        record(app, "search", url, detail);
        let _ = eval_string(app, "toolbar", "window.__C96_STATUS__&&window.__C96_STATUS__('搜索中…')");
        return;
    }
    if kind == "navigate" {
        set_query(v.get("query").and_then(|x| x.as_str()).unwrap_or(""));
        if !detail.is_empty() {
            let _ = raw_navigate(app, "main", detail);
        }
        record(app, "navigate", detail, "");
        return;
    }
    if kind == "cmd" {
        let cmd = detail.to_string();
        let a = app.clone();
        std::thread::spawn(move || {
            if cmd == "capture" {
                let _ = eval_string(&a, "main", "window.__C96_CAPTURE__&&window.__C96_CAPTURE__();");
            }
        });
        return;
    }
    if kind == "ui" {
        let op = detail.to_string();
        let a = app.clone();
        std::thread::spawn(move || {
            let _ = ui_command(&a, &op);
        });
        return;
    }
    if kind == "capture" {
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
        let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("raw");
        let sel = v.get("selector").and_then(|x| x.as_str()).unwrap_or("");
        let lr = v.get("link_ratio").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let chars = text.chars().count();
        // E7 闸门：整块都是链接的页面（导航页 / 搜索结果页 / 列表页）不进知识库
        if lr >= 0.7 && chars < 3000 {
            record(app, "store-skip", url, &format!("skip: link-ratio {:.0}% ({} 字, {} {})", lr * 100.0, chars, mode, sel));
            return;
        }
        let q = current_query();
        let saved = match crate::store::save_capture(app, url, title, text, &q, 200, mode) {
            Ok(msg) => {
                record(app, "store", url, &format!("{} · {} 链接{:.0}%{}", msg, mode, lr * 100.0, if sel.is_empty() { String::new() } else { format!(" · {}", sel) }));
                true
            }
            Err(e) => {
                record(app, "store-error", url, &e);
                false
            }
        };
        let a = app.clone();
        std::thread::spawn(move || {
            let msg = if saved { "已收录当前页面" } else { "收录失败" };
            let _ = eval_string(&a, "toolbar", &format!("window.__C96_STATUS__&&window.__C96_STATUS__({});", serde_json::to_string(msg).unwrap_or_default()));
        });
        return;
    }
    if kind == "rss" {
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
        let app2 = app.clone();
        let src = label.to_string();
        // 不能在 webview 事件回调里同步 eval（可能自锁），丢到线程里算完再回送
        std::thread::spawn(move || {
            let (ok, payload, note) = match crate::store::rss_op(&app2, &op, &args) {
                Ok(val) => (true, val.to_string(), "ok".to_string()),
                Err(e) => (false, serde_json::Value::String(e.clone()).to_string(), e),
            };
            record(&app2, "rss-op", &op, &note);
            let _ = eval_string(&app2, &src, &rss_reply_js(&id, ok, &payload));
            if op == "lang_set" {
                let _ = eval_string(&app2, "toolbar", &format!("window.__C96_SET_LANG__&&window.__C96_SET_LANG__({});", payload));
            }
        });
        return;
    }
    record(app, kind, url, detail);
}

pub fn list_events() -> Vec<BrowserEvent> {
    let v = events().lock().unwrap_or_else(|e| e.into_inner());
    return v.clone();
}

pub fn clear_events() -> usize {
    let mut v = events().lock().unwrap_or_else(|e| e.into_inner());
    let n = v.len();
    v.clear();
    return n;
}

/// 注入到自控窗口每次文档创建的钩子（E3 快照壳的 E1 版本）。
pub const SNAPSHOT_INIT: &str = r##"(function () {
  if (window.__C96_SNAPSHOT__) { return; }
  var api = { version: 1, installedAt: Date.now() };
  api.capture = function () {
    var doc = document;
    var links = [];
    if (doc.querySelectorAll) {
      var nodes = doc.querySelectorAll("a[href]");
      for (var i = 0; i < nodes.length && i < 200; i++) {
        links.push({ href: nodes[i].href, text: (nodes[i].textContent || "").trim().slice(0, 120) });
      }
    }
    return {
      title: doc.title,
      url: location.href,
      readyState: doc.readyState,
      text: doc.body ? doc.body.innerText : "",
      html: doc.documentElement ? doc.documentElement.outerHTML : "",
      links: links
    };
  };
  api.done = function () { return !!(document.readyState === "complete"); };
  window.__C96_SNAPSHOT__ = api;
})();"##;

/// E7：正文抽取（readability-lite，跑在活的 DOM 上）。
/// 与 Rust 侧 C3（src/fetch/html.rs，HTTP 抓取路径）同一思路：
///   score = 密度 × (1-链接密度)² × 权重；候选 = 语义容器 ∪ 段落聚类。
/// 区别在于这里直接读渲染后的 DOM（JS 站点的正文此时才存在），并且：
///   1) 打分与渲染都跳过 菜单/侧栏/推荐/广告/评论/页脚（含中文类名）
///   2) 渲染时丢掉「整块都是链接」的小块（导航项 / 标签云 / 相关阅读）
///   3) 全页都是链接时返回 mode=fallback，由 Rust 侧链接密度闸门决定不收录
pub const EXTRACT_INIT: &str = r##"(function () {
  if (window.__C96_EXTRACT__) { return; }
  var NEG = ["nav","menu","footer","sidebar","side-bar","comment","share","social","related","recomm",
    "advert","promo","breadcrumb","pagination","pager","copyright","subscribe","newsletter","login",
    "signup","cookie","banner","widget","toolbar","sponsor","searchbar","search-bar","topbar","top-bar",
    "nav-bar","tags","tag-list","hot-list","rank-list","qrcode","float","popup","modal","mask",
    "推荐","相关","广告","评论","登录","注册","下载","扫码","更多","热榜","排行","标签","导航","侧栏","页脚","猜你","推广"];
  var POS = ["article","content","post","entry","story","main","body","text","detail","read",
    "markdown","prose","正文","文章","news"];
  var SKIPTAGS = {SCRIPT:1,STYLE:1,NOSCRIPT:1,TEMPLATE:1,SVG:1,IFRAME:1,CANVAS:1,FORM:1,BUTTON:1,
    SELECT:1,OPTION:1,TEXTAREA:1,INPUT:1,NAV:1,HEADER:1,FOOTER:1,ASIDE:1,DIALOG:1,MENU:1,LINK:1,META:1};
  var BLOCKS = {P:1,DIV:1,SECTION:1,ARTICLE:1,MAIN:1,H1:1,H2:1,H3:1,H4:1,H5:1,H6:1,UL:1,OL:1,LI:1,
    BLOCKQUOTE:1,PRE:1,FIGURE:1,FIGCAPTION:1,DL:1,DT:1,DD:1,TD:1,TH:1,TR:1,TABLE:1,CAPTION:1,ADDRESS:1};
  var SEM = ["article","main","[role=main]","[itemprop~=articleBody]","[itemprop~=articlebody]",
    "#article-content","#articleContent","#js_content","#content","#main-content","#mainContent",
    "#detail",".article-content",".articleContent",".article-body",".articleBody",".post-content",
    ".postContent",".entry-content",".entryContent",".content-body",".contentBody",".artical-content",
    ".art_content",".rich_media_content",".RMContent",".news-content",".detail-content",".article",".post",".entry"];
  function nws(s) {
    var n = 0;
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c !== 32 && c !== 9 && c !== 10 && c !== 13) { n++; }
    }
    return n;
  }
  function label(el) {
    var c = "";
    try { c = el.className || ""; } catch (e) { c = ""; }
    if (typeof c !== "string") { c = (el.getAttribute ? el.getAttribute("class") : "") || ""; }
    return (c + " " + (el.id || "")).toLowerCase();
  }
  function hit(el, list) {
    var s = label(el);
    if (!s) { return false; }
    for (var i = 0; i < list.length; i++) { if (s.indexOf(list[i]) >= 0) { return true; } }
    return false;
  }
  function skipEl(el) {
    if (!el || !el.tagName) { return false; }
    if (SKIPTAGS[el.tagName]) { return true; }
    if (el.getAttribute && el.getAttribute("aria-hidden") === "true") { return true; }
    return hit(el, NEG);
  }
  function insideSkip(el, stop) {
    var cur = el;
    while (cur && cur !== stop) {
      if (skipEl(cur)) { return true; }
      cur = cur.parentElement;
    }
    return false;
  }
  function metrics(el) {
    var m = { tags: 0, link: 0, skipTxt: 0, text: 0 };
    m.text = nws(el.textContent || "");
    var kids = el.querySelectorAll ? el.querySelectorAll("*") : [];
    for (var i = 0; i < kids.length; i++) {
      var k = kids[i];
      m.tags++;
      if (skipEl(k)) { m.skipTxt += nws(k.textContent || ""); continue; }
      if (k.tagName === "A") { m.link += nws(k.textContent || ""); }
    }
    m.clean = m.text - Math.min(m.skipTxt, m.text);
    if (m.clean < 0) { m.clean = 0; }
    return m;
  }
  function scoreOf(el, plen) {
    var m = metrics(el);
    var base = Math.max(plen || 0, m.clean * 0.55);
    if (base < 120) { return null; }
    var ld = m.link / Math.max(m.text, 1);
    if (ld > 0.7) { return null; }
    var density = base / (m.tags + 1);
    var w = 1.0;
    if (hit(el, POS)) { w = w * 1.4; }
    var t = el.tagName;
    if (t === "ARTICLE" || t === "MAIN") { w = w * 1.15; }
    var ip = el.getAttribute ? (el.getAttribute("itemprop") || "").toLowerCase() : "";
    if (ip.indexOf("articlebody") >= 0) { w = w * 1.2; }
    if (t === "BODY" || t === "UL" || t === "OL" || t === "TABLE") { w = w * 0.8; }
    return { score: density * (1 - ld) * (1 - ld) * w, clean: m.clean, ld: ld, base: base, tags: m.tags };
  }
  function selectorOf(el) {
    var s = el.tagName.toLowerCase();
    if (el.id) { s = s + "#" + el.id; }
    var c = "";
    try { c = el.className || ""; } catch (e) { c = ""; }
    if (typeof c === "string" && c) { s = s + "." + c.replace(/^ +| +$/g, "").split(/ +/)[0]; }
    return s.slice(0, 60);
  }
  function linkRatioOf(el) {
    var m = metrics(el);
    return m.link / Math.max(m.text, 1);
  }
  function isLinkBox(k) {
    var t = k.tagName;
    if (t !== "UL" && t !== "OL" && t !== "DIV" && t !== "LI" && t !== "SECTION" && t !== "P") { return false; }
    var txt = nws(k.textContent || "");
    if (txt <= 0 || txt > 200) { return false; }
    var as = k.querySelectorAll ? k.querySelectorAll("a") : [];
    if (!as.length) { return false; }
    var lt = 0;
    for (var i = 0; i < as.length; i++) { lt += nws(as[i].textContent || ""); }
    return lt / txt >= 0.8;
  }
  function render(el, out) {
    if (!el) { return; }
    if (el.nodeType === 3) { out.text += el.nodeValue; return; }
    if (el.nodeType !== 1) { return; }
    if (el !== out.root && skipEl(el)) { return; }
    var block = !!BLOCKS[el.tagName];
    var t = out.text;
    if (block && t && t.charAt(t.length - 1) !== "\n") { out.text += "\n"; }
    var kids = el.childNodes;
    for (var i = 0; i < kids.length; i++) {
      var k = kids[i];
      if (k.nodeType === 1 && isLinkBox(k)) { continue; }
      render(k, out);
    }
    t = out.text;
    if (block && t && t.charAt(t.length - 1) !== "\n") { out.text += "\n"; }
    return;
  }
  function normalize(raw) {
    var lines = raw.split("\n");
    var out = [];
    var last = "";
    for (var i = 0; i < lines.length; i++) {
      var l = lines[i].replace(/[\t \u00a0\u3000]+/g, " ").replace(/^ +| +$/g, "");
      if (!l || l.length < 2) { continue; }
      if (l === last) { continue; }
      out.push(l);
      last = l;
    }
    return out.join("\n");
  }
  function extract() {
    var t0 = (new Date()).getTime();
    var body = document.body;
    if (!body) { return { text: "", chars: 0, linkRatio: 0, selector: "body?", mode: "empty", candidates: 0, ms: 0 }; }
    var plen = (typeof Map === "function") ? new Map() : null;
    var ps = body.getElementsByTagName("p");
    var pn = Math.min(ps.length, 1500);
    for (var i = 0; i < pn; i++) {
      var p = ps[i];
      var len = nws(p.textContent || "");
      if (len < 12) { continue; }
      if (insideSkip(p, body)) { continue; }
      var cur = p.parentElement;
      var depth = 0;
      while (cur && depth < 12) {
        if (plen) { plen.set(cur, (plen.get(cur) || 0) + len); }
        cur = cur.parentElement;
        depth++;
      }
    }
    var seen = [];
    var cands = [];
    function add(el) {
      if (!el || !el.tagName) { return; }
      if (seen.indexOf(el) >= 0) { return; }
      seen.push(el);
      cands.push(el);
    }
    for (var s = 0; s < SEM.length; s++) {
      var els = null;
      try { els = document.querySelectorAll(SEM[s]); } catch (e) { els = null; }
      if (!els) { continue; }
      for (var j = 0; j < els.length && j < 4; j++) { add(els[j]); }
    }
    if (plen) { plen.forEach(function (v, el) { if (v >= 120) { add(el); } }); }
    add(body);
    var best = body;
    var bestScore = -1;
    var bestPlen = 0;
    var bestClean = 0;
    for (var c = 0; c < cands.length; c++) {
      var el = cands[c];
      if (el !== body && insideSkip(el, null)) { continue; }
      var pl = plen ? (plen.get(el) || 0) : 0;
      var sc = scoreOf(el, pl);
      if (!sc) { continue; }
      if (sc.score > bestScore) { bestScore = sc.score; best = el; bestPlen = pl; bestClean = sc.clean; }
    }
    var mode = "fallback";
    if (bestScore >= 0) {
      mode = (bestPlen >= 120) ? "cluster" : "semantic";
      if (best.tagName === "ARTICLE") { mode = "article"; }
    }
    // 正文分散在多个 section 时单个候选会明显偏小：整页去掉导航后仍以正文为主（链接少）就退一级用 body
    if (best !== body) {
      var bm = metrics(body);
      var bld = bm.link / Math.max(bm.text, 1);
      if (bm.clean > bestClean * 3 && bld < 0.4) { best = body; mode = "fallback"; }
    }
    var out = { text: "", root: best };
    render(best, out);
    var text = normalize(out.text);
    return { text: text, chars: nws(text), linkRatio: linkRatioOf(best), selector: selectorOf(best),
      mode: mode, candidates: cands.length, ms: ((new Date()).getTime() - t0) };
  }
  window.__C96_EXTRACT__ = { version: 1, extract: extract, nws: nws, skipEl: skipEl };
})();"##;
/// E2：事件钩子。点击 / SPA / ready 通过 WebView2 原生 postMessage 回传。
pub const EVENTS_INIT: &str = r##"(function () {
  if (window.__C96_EVENTS__) { return; }
  window.__C96_EVENTS__ = true;
  function post(type, detail) {
    var payload = JSON.stringify({ type: type, url: location.href, detail: detail });
    try {
      if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
        window.chrome.webview.postMessage(payload);
        return;
      }
    } catch (e) {}
    try {
      if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
        window.__TAURI_INTERNALS__.invoke("browser_event", { payload: payload });
      }
    } catch (e) {}
  }
  function describe(el) {
    if (!el || !el.tagName) { return ""; }
    var t = el.tagName.toLowerCase();
    var txt = (el.innerText || el.textContent || "").trim().slice(0, 80);
    var href = el.href || "";
    return t + " | " + href + " | " + txt;
  }
  document.addEventListener("click", function (e) {
    var a = null;
    if (e.target && e.target.closest) { a = e.target.closest("a"); }
    post("click", describe(a || e.target));
  }, true);
  window.addEventListener("hashchange", function () { post("spa", "hashchange"); });
  window.addEventListener("popstate", function () { post("spa", "popstate"); });
  var last = location.href;
  setInterval(function () {
    if (location.href !== last) { last = location.href; post("spa", "url-changed"); }
  }, 800);
  post("ready", document.title);
})();"##;
/// E6：链接点击修正 —— WebView2 默认**丢弃**新窗口请求，导致 target=_blank / window.open 点击"没反应"。
/// 统一改写成当前窗口导航（单窗口浏览器语义），这样结果页链接一点就开，且自动 RAG 采集照常工作。
pub const LINKS_INIT: &str = r##"(function () {
  if (window.__C96_LINKS__) { return; }
  window.__C96_LINKS__ = true;
  function selfTarget(el) {
    if (!el || !el.getAttribute) { return; }
    var t = (el.getAttribute("target") || "").toLowerCase();
    if (t && t !== "_self" && t !== "_top" && t !== "_parent") { el.setAttribute("target", "_self"); }
  }
  document.addEventListener("click", function (e) {
    var a = null;
    try { if (e.target && e.target.closest) { a = e.target.closest("a"); } } catch (err) {}
    if (a) { selfTarget(a); }
  }, true);
  document.addEventListener("auxclick", function (e) {
    if (e.button !== 1) { return; }
    var a = null;
    try { if (e.target && e.target.closest) { a = e.target.closest("a"); } } catch (err) {}
    if (a && a.href) { e.preventDefault(); location.href = a.href; }
  }, true);
  document.addEventListener("submit", function (e) { selfTarget(e.target); }, true);
  var origOpen = window.open;
  window.open = function (url) {
    try {
      if (url) { location.href = String(url); }
    } catch (err) {}
    return null;
  };
})();"##;

/// E3：主窗口叠加条 —— C96 搜索/网址输入 + 页面加载后自动 capture 回传 Rust 入库。
pub const OVERLAY_INIT: &str = r##"(function () {
  if (window.__C96_BAR__) { return; }
  window.__C96_BAR__ = true;
  var MODE = window.__C96_MODE__ || "content";
  var UI_LANG = "zh";
  try { var _cached = localStorage.getItem("c96.uiLang"); if (_cached === "en") { UI_LANG = "en"; } } catch (e) {}
  var I18N = {
    zh: {
      search_ph: "输入搜索内容或网址，回车（加载后自动存入 RAG 知识库）",
      ready: "就绪", snap: "收录", snap_title: "手动收录当前页面的文章信息",
      settings: "设置", settings_title: "设置", ui_lang: "界面语言",
      lang_hint: "选择后立即生效，并记住到下次启动。", close: "关闭",
      rss_title: "RSS 内容管理器（增删改查）", rss_panel_title: "RSS 内容管理器",
      refresh: "刷新", loading: "加载中…", add_sub: "添加订阅",
      kw_ph: "关键词：新订阅会立刻搜索并入 RAG", interval_title: "刷新间隔（秒）",
      topics: "关键词订阅 ", sources: "来源分组 ", all_content: "全部内容 ",
      filter_ph: "过滤 标题/网址/主机/来源", only_manual: "只看随手收录", show_all: "显示全部",
      hosts: "主机制 ", blocklist: "URL 黑名单 ",
      block_ph: "加入黑名单：*.tracker.net 或 *://news.example.com/*（支持 *）", block_add: "加入黑名单",
      del: "删", confirm: "确认?", delete: "删除", confirm_delete: "确认删除?",
      items: "条目", save_interval: "改间隔", pause: "暂停", resume: "恢复",
      clear: "清空", confirm_clear: "确认清空?", copy: "复制", copy_url: "复制地址",
      ignore: "忽略", unignore: "取消忽略", builtin: "内置"
    },
    en: {
      search_ph: "Search or enter a URL, press Enter (auto-saved to RAG after load)",
      ready: "Ready", snap: "Capture", snap_title: "Manually save this page's article",
      settings: "Settings", settings_title: "Settings", ui_lang: "UI language",
      lang_hint: "Applied immediately and remembered for next launch.", close: "Close",
      rss_title: "RSS content manager (CRUD)", rss_panel_title: "RSS Content Manager",
      refresh: "Refresh", loading: "Loading…", add_sub: "Add",
      kw_ph: "Keyword: new subscription searches and saves to RAG immediately", interval_title: "Refresh interval (seconds)",
      topics: "Keyword subscriptions ", sources: "Source groups ", all_content: "All content ",
      filter_ph: "Filter title/URL/host/source", only_manual: "Manual only", show_all: "Show all",
      hosts: "Hosts ", blocklist: "URL blocklist ",
      block_ph: "Add block rule: *.tracker.net or *://news.example.com/* (supports *)", block_add: "Block",
      del: "Del", confirm: "Confirm?", delete: "Delete", confirm_delete: "Confirm delete?",
      items: "Items", save_interval: "Save", pause: "Pause", resume: "Resume",
      clear: "Clear", confirm_clear: "Confirm clear?", copy: "Copy", copy_url: "Copy URL",
      ignore: "Ignore", unignore: "Unignore", builtin: "Built-in"
    }
  };
  function L(k) {
    var d = I18N[UI_LANG] || I18N.zh;
    if (d[k] !== undefined) { return d[k]; }
    return I18N.zh[k] !== undefined ? I18N.zh[k] : k;
  }
  function applyLang() {
    var q = document.getElementById("c96-q");
    if (q) { q.placeholder = L("search_ph"); }
    var st = document.getElementById("c96-status");
    if (st && st.getAttribute("data-l10n") === "ready") { st.textContent = L("ready"); }
    var snap = document.getElementById("c96-snap-btn");
    if (snap) {
      snap.title = L("snap_title");
      var sl = snap.querySelector(".c96-btn-label");
      if (sl) { sl.textContent = L("snap"); }
    }
    var rss = document.getElementById("c96-rss-btn");
    if (rss) { rss.title = L("rss_title"); }
    var setb = document.getElementById("c96-settings-btn");
    if (setb) {
      setb.title = L("settings_title");
      var pl = setb.querySelector(".c96-btn-label");
      if (pl) { pl.textContent = L("settings"); }
    }
    var d = document.getElementById("c96-settings");
    if (d) {
      var ht = d.querySelector(".c96-set-title"); if (ht) { ht.textContent = L("settings_title"); }
      var lb = d.querySelector(".c96-set-label"); if (lb) { lb.textContent = L("ui_lang"); }
      var hn = d.querySelector(".c96-set-hint"); if (hn) { hn.textContent = L("lang_hint"); }
      var cl = d.querySelector(".c96-set-close"); if (cl) { cl.textContent = L("close"); }
    }
    var p = document.getElementById("c96-rss-panel");
    if (p && p.parentNode) { p.parentNode.removeChild(p); }
  }
  function setLang(l) {
    UI_LANG = (l === "en") ? "en" : "zh";
    try { localStorage.setItem("c96.uiLang", UI_LANG); } catch (e) {}
    applyLang();
  }
  function syncLang() {
    try {
      ipc("lang_get", {}).then(function (v) {
        if ((v === "en" || v === "zh") && v !== UI_LANG) {
          UI_LANG = v;
          try { localStorage.setItem("c96.uiLang", v); } catch (e) {}
          applyLang();
        }
      }).catch(function () {});
    } catch (e) {}
  }
  function mkBarBtn(id, title, color, svg, label) {
    var b = document.createElement("button");
    b.id = id;
    b.type = "button";
    b.title = title;
    b.setAttribute("style", "display:flex;align-items:center;gap:4px;height:30px;padding:0 8px;background:#1c2027;color:" + color + ";border:1px solid #333a45;border-radius:6px;cursor:pointer;font:12px system-ui;");
    b.innerHTML = svg;
    var s = document.createElement("span");
    s.className = "c96-btn-label";
    s.textContent = label;
    b.appendChild(s);
    return b;
  }
  function openSettings() {
    if (MODE === "toolbar") { post("ui", "settings"); return null; }
    var d = document.getElementById("c96-settings");
    if (!d) {
      d = document.createElement("div");
      d.id = "c96-settings";
      d.setAttribute("style", "display:none;position:fixed;top:0;left:0;right:0;bottom:0;z-index:2147483647;background:transparent;align-items:center;justify-content:center;font:13px/1.5 system-ui,'Segoe UI',sans-serif;");
      var card = document.createElement("div");
      card.setAttribute("style", "width:360px;max-width:92vw;background:#14161a;color:#e6e6e6;border:1px solid #2a2e36;border-radius:10px;box-shadow:0 18px 50px rgba(0,0,0,.6);padding:16px;");
      var h = document.createElement("div");
      h.className = "c96-set-title";
      h.textContent = L("settings_title");
      h.setAttribute("style", "color:#f59e0b;font-weight:700;margin-bottom:12px;");
      var lb = document.createElement("div");
      lb.className = "c96-set-label";
      lb.textContent = L("ui_lang");
      lb.setAttribute("style", "color:#9aa4b2;margin-bottom:6px;");
      var sel = document.createElement("select");
      sel.id = "c96-lang-sel";
      sel.setAttribute("style", "width:100%;height:32px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 8px;font:13px system-ui;");
      var zh = document.createElement("option");
      zh.value = "zh"; zh.textContent = "中文（简体）";
      var en = document.createElement("option");
      en.value = "en"; en.textContent = "English";
      sel.appendChild(zh); sel.appendChild(en);
      sel.value = UI_LANG;
      sel.addEventListener("change", function () {
        setLang(sel.value);
        try { ipc("lang_set", { lang: sel.value }).catch(function () {}); } catch (e) {}
      });
      var hint = document.createElement("div");
      hint.className = "c96-set-hint";
      hint.textContent = L("lang_hint");
      hint.setAttribute("style", "color:#6b7280;font-size:12px;margin-top:8px;");
      var foot = document.createElement("div");
      foot.setAttribute("style", "display:flex;justify-content:flex-end;margin-top:14px;");
      var close = document.createElement("button");
      close.type = "button";
      close.className = "c96-set-close";
      close.textContent = L("close");
      close.setAttribute("style", "height:30px;padding:0 14px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;cursor:pointer;font:12px system-ui;");
      close.addEventListener("click", function () { d.style.display = "none"; if (MODE === "overlay") { post("ui", "close"); } });
      d.addEventListener("click", function (ev) { if (ev.target === d) { d.style.display = "none"; if (MODE === "overlay") { post("ui", "close"); } } });
      foot.appendChild(close);
      card.appendChild(h); card.appendChild(lb); card.appendChild(sel); card.appendChild(hint); card.appendChild(foot);
      d.appendChild(card);
      (document.body || document.documentElement).appendChild(d);
    }
    d.style.display = "flex";
    var cur = document.getElementById("c96-lang-sel");
    if (cur) { cur.value = UI_LANG; }
    return d;
  }
  function post(type, detail, extra) {
    var payload = { type: type, url: location.href, detail: detail };
    if (extra) { for (var k in extra) { if (extra.hasOwnProperty(k)) { payload[k] = extra[k]; } } }
    var s = JSON.stringify(payload);
    try {
      if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
        window.chrome.webview.postMessage(s);
        return;
      }
    } catch (e) {}
    try {
      if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
        window.__TAURI_INTERNALS__.invoke("browser_event", { payload: s });
      }
    } catch (e) {}
  }
  function installBar() {
    if (document.getElementById("c96-bar")) { return; }
    var bar = document.createElement("div");
    bar.id = "c96-bar";
    bar.setAttribute("style", "position:fixed;top:0;left:0;right:0;height:46px;z-index:2147483647;background:#14161a;border-bottom:1px solid #2a2e36;display:flex;align-items:center;gap:8px;padding:0 10px;font:13px/1.4 system-ui,Segoe UI,sans-serif;box-sizing:border-box;");
    var label = document.createElement("span");
    label.textContent = "C96";
    label.setAttribute("style", "color:#93c5fd;font-weight:700;letter-spacing:.5px;");
    var input = document.createElement("input");
    input.id = "c96-q";
    input.type = "text";
    input.placeholder = L("search_ph");
    input.setAttribute("style", "flex:1;height:32px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 10px;font:inherit;outline:none;");
    var status = document.createElement("span");
    status.id = "c96-status";
    status.textContent = L("ready");
    status.setAttribute("data-l10n", "ready");
    status.setAttribute("style", "color:#9aa4b2;font-size:12px;max-width:260px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;");
    input.addEventListener("keydown", function (e) {
      if (e.key !== "Enter") { return; }
      var q = input.value.trim();
      if (!q) { return; }
      var target = (q.indexOf("http://") === 0 || q.indexOf("https://") === 0)
        ? q
        : "https://www.bing.com/search?q=" + encodeURIComponent(q);
      post("search", q, { target: target });
      status.textContent = "搜索中…";
    });
    bar.appendChild(label);
    bar.appendChild(input);
    bar.appendChild(status);
    // 收录：手动收录当前页面文章（复用自动采集的抽取 + 入库链路）
    var snapBtn = mkBarBtn("c96-snap-btn", L("snap_title"), "#93c5fd",
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#93c5fd" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z"/><circle cx="12" cy="13" r="4"/></svg>',
      L("snap"));
    snapBtn.addEventListener("click", function (e) {
      e.preventDefault();
      e.stopPropagation();
      if (MODE === "toolbar") {
        var st = document.getElementById("c96-status");
        if (st) { st.textContent = "收录中…"; }
        post("cmd", "capture");
      } else {
        capture();
      }
    });
    bar.appendChild(snapBtn);
    // RSS 内容管理器：顶部工具栏图标 -> 弹层（增删改查）
    var rssBtn = mkBarBtn("c96-rss-btn", L("rss_title"), "#f59e0b",
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#f59e0b" stroke-width="2.4" stroke-linecap="round"><path d="M4 11a9 9 0 0 1 9 9"/><path d="M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1.5" fill="#f59e0b" stroke="none"/></svg>',
      "RSS");
    rssBtn.addEventListener("click", function (e) {
      e.preventDefault();
      e.stopPropagation();
      toggleRssPanel();
    });
    bar.appendChild(rssBtn);
    // 设置：UI 语言（中文 / English），选择后立即生效并持久化
    var setBtn = mkBarBtn("c96-settings-btn", L("settings_title"), "#93c5fd",
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#93c5fd" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h.01a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v.01a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>',
      L("settings"));
    setBtn.addEventListener("click", function (e) {
      e.preventDefault();
      e.stopPropagation();
      openSettings();
    });
    bar.appendChild(setBtn);
    var root = document.body || document.documentElement;
    root.appendChild(bar);
    input.focus();
  }
  // E7：自动入库的正文抽取 —— 不再用整页 innerText（会把菜单/侧栏/推荐一起摘进来）。
  // 抽取交给 __C96_EXTRACT__（EXTRACT_INIT）；读不到时退化回 innerText（mode=raw）。
  function extractPage() {
    var ex = window.__C96_EXTRACT__;
    if (ex && ex.extract) {
      try { return ex.extract(); } catch (e) {}
    }
    var t = "";
    try { t = document.body ? document.body.innerText : ""; } catch (e) {}
    return { text: t, chars: t.length, linkRatio: 0, selector: "", mode: "raw" };
  }
  var CAP = { url: "", seq: 0, last: -1, tries: 0, stable: 0, fired: false };
  function capture() {
    var bar = document.getElementById("c96-bar");
    var status = document.getElementById("c96-status");
    if (bar) { bar.style.display = "none"; }
    var r = extractPage();
    var text = r.text || "";
    var title = document.title || "";
    if (bar) { bar.style.display = "flex"; }
    if (status) {
      var tag = r.mode + (r.selector ? " " + r.selector : "") + (r.linkRatio ? " · 链接 " + Math.round(r.linkRatio * 100) + "%" : "");
      status.textContent = "已送存 " + String(text.length) + " 字（" + tag + "）";
    }
    post("capture", "", { title: title, text: text, mode: r.mode, selector: r.selector, link_ratio: r.linkRatio, chars: r.chars });
  }
  // 抢在正文渲染出来之前抓会把菜单存进去：等到「文本长度稳定」再送存，最多等 8 次。
  function tick(seq) {
    if (seq !== CAP.seq || CAP.fired) { return; }
    CAP.tries = CAP.tries + 1;
    var r = extractPage();
    var n = (typeof r.chars === "number" && r.chars >= 0) ? r.chars : (r.text || "").length;
    var tol = Math.max(30, Math.round(n * 0.02));
    if (n >= 400 && Math.abs(n - CAP.last) <= tol) { CAP.stable = CAP.stable + 1; } else { CAP.stable = 0; }
    CAP.last = n;
    if ((n >= 400 && CAP.stable >= 1) || CAP.tries >= 6) { CAP.fired = true; capture(); return; }
    setTimeout(function () { tick(seq); }, CAP.tries < 3 ? 800 : 1400);
  }
  function beginCapture() {
    CAP.url = location.href;
    CAP.seq = CAP.seq + 1;
    CAP.last = -1;
    CAP.tries = 0;
    CAP.stable = 0;
    CAP.fired = false;
    var seq = CAP.seq;
    setTimeout(function () { tick(seq); }, 1000);
  }
  function watchNav() {
    if (location.href !== CAP.url) { beginCapture(); }
  }
  function describe(el) {
    if (!el || !el.tagName) { return ""; }
    var txt = (el.innerText || el.textContent || "").trim().slice(0, 80);
    return el.tagName.toLowerCase() + " | " + (el.href || "") + " | " + txt;
  }
  document.addEventListener("click", function (e) {
    var a = null;
    if (e.target && e.target.closest) { a = e.target.closest("a"); }
    if (a) { post("click", describe(a)); }
  }, true);
  window.addEventListener("hashchange", function () { post("spa", "hashchange"); });
  window.addEventListener("popstate", function () { post("spa", "popstate"); });
  // ---- RSS 内容管理器（顶部工具栏图标 -> 弹层）：查列表 / 增订阅 / 改间隔 / 删订阅与条目 ----
  var RSS_BASE = ["", "", ""];
  var RSS_PENDING = {};
  var RSS_SEQ = 0;
  var RSS_DQ_TIMER = 0;
  var RSS_OP = { topic_query: "topic_add", topic_list: "topics", rss_sources: "sources", rss_base: "base", topic_docs: "topic_docs", topic_run: "topic_run", topic_remove: "topic_remove", topic_update: "topic_update", rss_doc_remove: "doc_remove" };
  function rssReply(id, ok, data) {
    var p = RSS_PENDING[id];
    if (!p) { return; }
    delete RSS_PENDING[id];
    var val = data;
    try { val = JSON.parse(data); } catch (e) {}
    if (ok) { p.ok(val); } else { p.bad(new Error(typeof val === "string" ? val : JSON.stringify(val))); }
  }
  // 外部网页 / file:// 的 Origin 不被 Tauri IPC 接受（实测 "Origin header is not a valid URL"），
  // 所以走 WebView2 原生 postMessage：Rust 侧 rss_op() 算完用 eval 回送 reply()。
  function ipc(cmd, args) {
    var op = RSS_OP[cmd] || cmd;
    return new Promise(function (resolve, reject) {
      var id = "r" + (++RSS_SEQ);
      var payload = JSON.stringify({ type: "rss", id: id, op: op, args: args || {} });
      var sent = false;
      try {
        if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
          window.chrome.webview.postMessage(payload);
          sent = true;
        }
      } catch (e) {}
      if (!sent) { reject(new Error("没有可用的页面通道（需 WebView2）")); return; }
      RSS_PENDING[id] = { ok: resolve, bad: reject };
      setTimeout(function () {
        if (RSS_PENDING[id]) { delete RSS_PENDING[id]; reject(new Error("超时：" + op)); }
      }, 60000);
    });
  }
  function rssStatus(msg) {
    var el = document.getElementById("c96-rss-status");
    if (el) { el.textContent = msg; }
  }
  function rssBtnNew(text, id, color) {
    var b = document.createElement("button");
    b.type = "button";
    b.textContent = text;
    if (id) { b.id = id; }
    b.setAttribute("style", "height:24px;padding:0 8px;background:#1c2027;color:" + (color || "#e6e6e6") + ";border:1px solid #333a45;border-radius:5px;cursor:pointer;font:12px system-ui;");
    return b;
  }
  function rssTime(ms) {
    try { return new Date(ms).toLocaleString(); } catch (e) { return String(ms); }
  }
  function rssCopy(text) {
    try {
      var t = document.createElement("textarea");
      t.value = text;
      t.setAttribute("style", "position:fixed;left:-9999px;top:0;");
      (document.body || document.documentElement).appendChild(t);
      t.select();
      var ok = document.execCommand("copy");
      if (t.parentNode) { t.parentNode.removeChild(t); }
      rssStatus(ok ? "已复制 " + text : "复制失败：" + text);
    } catch (e) { rssStatus("复制失败：" + text); }
  }
  function rssUrl(base, path, sync) {
    return "http://" + base + path + (sync ? "?t=" + sync : "");
  }
  function installRssPanel() {
    if (document.getElementById("c96-rss-panel")) { return; }
    var p = document.createElement("div");
    p.id = "c96-rss-panel";
    p.setAttribute("style", "display:none;position:fixed;top:54px;right:12px;width:620px;max-width:94vw;max-height:78vh;overflow:auto;z-index:2147483647;background:#14161a;color:#e6e6e6;border:1px solid #2a2e36;border-radius:10px;box-shadow:0 14px 44px rgba(0,0,0,.6);font:13px/1.55 system-ui,'Segoe UI',sans-serif;padding:12px;");
    var head = document.createElement("div");
    head.setAttribute("style", "display:flex;align-items:center;gap:8px;margin-bottom:8px;");
    var title = document.createElement("b");
    title.textContent = L("rss_panel_title");
    title.setAttribute("style", "color:#f59e0b;");
    var st = document.createElement("span");
    st.id = "c96-rss-status";
    st.textContent = L("ready");
    st.setAttribute("style", "flex:1;color:#9aa4b2;font-size:12px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;");
    var reload = rssBtnNew(L("refresh"), "c96-rss-reload");
    var close = rssBtnNew("×", "c96-rss-close", "#f87171");
    close.addEventListener("click", function () { p.style.display = "none"; if (MODE === "overlay") { post("ui", "close"); } });
    reload.addEventListener("click", function () { reloadRss(); });
    head.appendChild(title);
    head.appendChild(st);
    head.appendChild(reload);
    head.appendChild(close);
    var urls = document.createElement("div");
    urls.id = "c96-rss-urls";
    urls.setAttribute("style", "background:#1a1d23;border:1px solid #2a2e36;border-radius:8px;padding:8px;font-size:12px;color:#9aa4b2;");
    urls.textContent = L("loading");
    var addBox = document.createElement("div");
    addBox.setAttribute("style", "display:flex;gap:6px;margin:8px 0;");
    var q = document.createElement("input");
    q.id = "c96-rss-new-q";
    q.placeholder = L("kw_ph");
    q.setAttribute("style", "flex:1;height:28px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 8px;font:12px system-ui;");
    var iv = document.createElement("input");
    iv.id = "c96-rss-new-int";
    iv.value = "300";
    iv.title = L("interval_title");
    iv.setAttribute("style", "width:64px;height:28px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 6px;font:12px system-ui;");
    var add = rssBtnNew(L("add_sub"), "c96-rss-add", "#93c5fd");
    add.addEventListener("click", function () {
      var text = (q.value || "").trim();
      if (!text) { rssStatus("请输入关键词"); return; }
      var secs = parseInt(iv.value, 10);
      if (!(secs > 0)) { secs = 300; }
      rssStatus("搜索入库中：" + text + " …");
      ipc("topic_query", { query: text, cache_only: false, cacheOnly: false }).then(function (r) {
        var hits = (r && r.hits) ? r.hits.length : 0;
        return ipc("topic_update", { query: text, interval_secs: secs, intervalSecs: secs, paused: false }).then(function () {
          rssStatus("已新增订阅「" + text + "」命中 " + hits + " 条 · 间隔 " + secs + "s");
          q.value = "";
          return reloadRss();
        });
      }).catch(function (e) { rssStatus("新增失败：" + e); });
    });
    addBox.appendChild(q);
    addBox.appendChild(iv);
    addBox.appendChild(add);
    var tTitle = document.createElement("div");
    tTitle.setAttribute("style", "color:#93c5fd;margin:10px 0 4px;");
    tTitle.textContent = L("topics");
    var tCount = document.createElement("span");
    tCount.id = "c96-rss-tcount";
    tCount.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    tTitle.appendChild(tCount);
    var tList = document.createElement("div");
    tList.id = "c96-rss-topics";
    tList.textContent = L("loading");
    var sTitle = document.createElement("div");
    sTitle.setAttribute("style", "color:#93c5fd;margin:12px 0 4px;");
    sTitle.textContent = L("sources");
    var sCount = document.createElement("span");
    sCount.id = "c96-rss-scount";
    sCount.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    sTitle.appendChild(sCount);
    var sList = document.createElement("div");
    sList.id = "c96-rss-sources";
    sList.textContent = L("loading");
    // 全部内容：过滤 + 只看随手收录 + 逐条删
    var dTitle = document.createElement("div");
    dTitle.setAttribute("style", "color:#93c5fd;margin:12px 0 4px;");
    dTitle.textContent = L("all_content");
    var dCount = document.createElement("span");
    dCount.id = "c96-rss-dcount";
    dCount.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    dTitle.appendChild(dCount);
    var dCtl = document.createElement("div");
    dCtl.setAttribute("style", "display:flex;gap:6px;margin:2px 0 4px;");
    var dq = document.createElement("input");
    dq.id = "c96-rss-dq";
    dq.placeholder = L("filter_ph");
    dq.setAttribute("style", "flex:1;height:26px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 8px;font:12px system-ui;");
    dq.addEventListener("input", function () {
      if (RSS_DQ_TIMER) { clearTimeout(RSS_DQ_TIMER); }
      RSS_DQ_TIMER = setTimeout(function () { RSS_DQ_TIMER = 0; reloadDocs(); }, 250);
    });
    var dunsub = rssBtnNew(L("only_manual"), "c96-rss-dunsub", "#fbbf24");
    dunsub.setAttribute("data-on", "0");
    dunsub.addEventListener("click", function () {
      var on = this.getAttribute("data-on") === "1";
      this.setAttribute("data-on", on ? "0" : "1");
      this.textContent = on ? L("only_manual") : L("show_all");
      reloadDocs();
    });
    dCtl.appendChild(dq);
    dCtl.appendChild(dunsub);
    var dList = document.createElement("div");
    dList.id = "c96-rss-docs";
    dList.textContent = L("loading");
    // 主机制：按站点清空 / 忽略（忽略后不再自动收录）
    var hTitle = document.createElement("div");
    hTitle.setAttribute("style", "color:#93c5fd;margin:12px 0 4px;");
    hTitle.textContent = L("hosts");
    var hCount = document.createElement("span");
    hCount.id = "c96-rss-hcount";
    hCount.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    hTitle.appendChild(hCount);
    var hList = document.createElement("div");
    hList.id = "c96-rss-hosts";
    hList.textContent = L("loading");
    // 黑名单：内置搜索引擎结果页规则 + 用户自加 glob（含此 URL 的页面不再自动收录）
    var bTitle = document.createElement("div");
    bTitle.setAttribute("style", "color:#93c5fd;margin:12px 0 4px;");
    bTitle.textContent = L("blocklist");
    var bCount = document.createElement("span");
    bCount.id = "c96-rss-bcount";
    bCount.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    bTitle.appendChild(bCount);
    var bAddBox = document.createElement("div");
    bAddBox.setAttribute("style", "display:flex;gap:6px;margin:2px 0 4px;");
    var bp = document.createElement("input");
    bp.id = "c96-rss-bpat";
    bp.placeholder = L("block_ph");
    bp.setAttribute("style", "flex:1;height:26px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:6px;padding:0 8px;font:12px system-ui;");
    var badd = rssBtnNew(L("block_add"), "c96-rss-badd", "#93c5fd");
    badd.addEventListener("click", function () {
      var raw = (bp.value || "").trim();
      if (!raw) { rssStatus("请输入黑名单规则"); return; }
      ipc("pattern_add", { pattern: raw }).then(function (ok) {
        rssStatus(ok ? "已加入黑名单：" + raw + "（含此 URL 的页面不再自动收录）" : "该规则已存在");
        bp.value = "";
        return reloadPatterns();
      }).catch(function (e) { rssStatus("加入黑名单失败：" + e); });
    });
    bAddBox.appendChild(bp);
    bAddBox.appendChild(badd);
    var bList = document.createElement("div");
    bList.id = "c96-rss-block";
    bList.textContent = L("loading");
    p.appendChild(head);
    p.appendChild(urls);
    p.appendChild(addBox);
    p.appendChild(tTitle);
    p.appendChild(tList);
    p.appendChild(dTitle);
    p.appendChild(dCtl);
    p.appendChild(dList);
    p.appendChild(hTitle);
    p.appendChild(hList);
    p.appendChild(bTitle);
    p.appendChild(bAddBox);
    p.appendChild(bList);
    p.appendChild(sTitle);
    p.appendChild(sList);
    (document.body || document.documentElement).appendChild(p);
  }
  function rssItemLine(r) {
    var line = document.createElement("div");
    line.setAttribute("style", "display:flex;align-items:center;gap:6px;font-size:12px;padding:2px 0;");
    var when = document.createElement("span");
    when.textContent = rssTime(r[4]);
    when.setAttribute("style", "color:#6b7280;white-space:nowrap;");
    var a = document.createElement("a");
    a.href = r[1];
    a.textContent = r[2] || r[1];
    a.title = r[1];
    a.setAttribute("style", "flex:1;color:#93c5fd;text-decoration:none;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;");
    var host = document.createElement("span");
    host.textContent = r[3] || "";
    host.setAttribute("style", "color:#6b7280;");
    var del = rssBtnNew(L("del"), null, "#f87171");
    del.addEventListener("click", function (ev) {
      var b = ev.currentTarget;
      if (b.getAttribute("data-armed") !== "1") {
        b.setAttribute("data-armed", "1");
        b.textContent = L("confirm");
        return;
      }
      ipc("rss_doc_remove", { hash: r[0] }).then(function (ok) {
        line.style.display = "none";
        rssStatus(ok ? "已删条目 " + (r[2] || r[0]) : "条目不存在（可能已被 TTL 清掉）");
      }).catch(function (e) { rssStatus("删条目失败：" + e); });
    });
    line.appendChild(when);
    line.appendChild(a);
    line.appendChild(host);
    line.appendChild(del);
    return line;
  }
  function renderRssItems(box, rows) {
    box.textContent = "";
    if (!rows || rows.length === 0) { box.textContent = "（没有条目：点「刷新」跑一次订阅搜索）"; return; }
    for (var i = 0; i < rows.length; i++) { box.appendChild(rssItemLine(rows[i])); }
  }
  function rssTopicRow(t) {
    var wrap = document.createElement("div");
    wrap.setAttribute("style", "border:1px solid #2a2e36;border-radius:8px;padding:6px 8px;margin:5px 0;background:#1a1d23;");
    var head = document.createElement("div");
    head.setAttribute("style", "display:flex;align-items:center;gap:6px;");
    var name = document.createElement("span");
    name.textContent = "「" + t.query + "」";
    name.setAttribute("style", "font-weight:600;flex:1;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;");
    var meta = document.createElement("span");
    meta.textContent = "间隔 " + t.interval_secs + "s · runs " + t.runs + " · 入库 " + t.stored + (t.paused ? " · 已暂停" : "");
    meta.setAttribute("style", "color:#9aa4b2;font-size:12px;");
    head.appendChild(name);
    head.appendChild(meta);
    var ops = document.createElement("div");
    ops.setAttribute("style", "display:flex;gap:5px;flex-wrap:wrap;margin-top:5px;align-items:center;");
    var run = rssBtnNew(L("refresh"), null, "#93c5fd");
    run.addEventListener("click", function () {
      rssStatus("刷新中：" + t.query + " …");
      ipc("topic_run", { query: t.query }).then(function (r) {
        rssStatus("已刷新「" + t.query + "」新增 " + ((r && r.stored) ? r.stored : 0) + " 条");
        return reloadRss();
      }).catch(function (e) { rssStatus("刷新失败：" + e); });
    });
    var pause = rssBtnNew(t.paused ? L("resume") : L("pause"), null, "#fbbf24");
    pause.addEventListener("click", function () {
      ipc("topic_update", { query: t.query, interval_secs: 0, intervalSecs: 0, paused: !t.paused }).then(function () {
        rssStatus((t.paused ? "已恢复 " : "已暂停 ") + t.query);
        return reloadRss();
      }).catch(function (e) { rssStatus("操作失败：" + e); });
    });
    var int = document.createElement("input");
    int.id = "c96-rss-int-" + t.key;
    int.value = String(t.interval_secs);
    int.title = "刷新间隔（秒）";
    int.setAttribute("style", "width:64px;height:24px;background:#1c2027;color:#e6e6e6;border:1px solid #333a45;border-radius:5px;padding:0 6px;font:12px system-ui;");
    var save = rssBtnNew(L("save_interval"), "c96-rss-save-" + t.key, "#86efac");
    save.addEventListener("click", function () {
      var v = parseInt(int.value, 10);
      if (!(v > 0)) { rssStatus("间隔要大于 0 秒"); return; }
      ipc("topic_update", { query: t.query, interval_secs: v, intervalSecs: v, paused: null }).then(function () {
        rssStatus("「" + t.query + "」间隔改为 " + v + "s");
        return reloadRss();
      }).catch(function (e) { rssStatus("改间隔失败：" + e); });
    });
    var docs = rssBtnNew(L("items"), "c96-rss-docs-" + t.key);
    var box = document.createElement("div");
    box.id = "c96-rss-items-" + t.key;
    box.setAttribute("style", "display:none;margin-top:6px;border-top:1px dashed #2a2e36;padding-top:6px;");
    docs.addEventListener("click", function () {
      if (box.style.display === "none") {
        box.style.display = "block";
        box.textContent = L("loading");
        ipc("topic_docs", { query: t.query, limit: 20 }).then(function (rows) { renderRssItems(box, rows); })
          .catch(function (e) { box.textContent = "读取失败：" + e; });
      } else { box.style.display = "none"; }
    });
    var del = rssBtnNew(L("delete"), "c96-rss-del-" + t.key, "#f87171");
    del.addEventListener("click", function (ev) {
      var b = ev.currentTarget;
      if (b.getAttribute("data-armed") !== "1") {
        b.setAttribute("data-armed", "1");
        b.textContent = L("confirm_delete");
        return;
      }
      ipc("topic_remove", { query: t.query }).then(function (ok) {
        rssStatus(ok ? "已删除订阅「" + t.query + "」" : "未找到订阅「" + t.query + "」");
        return reloadRss();
      }).catch(function (e) { rssStatus("删除失败：" + e); });
    });
    ops.appendChild(run);
    ops.appendChild(pause);
    ops.appendChild(int);
    ops.appendChild(save);
    ops.appendChild(docs);
    ops.appendChild(del);
    wrap.appendChild(head);
    wrap.appendChild(ops);
    wrap.appendChild(box);
    return wrap;
  }
  function renderRssTopics(list) {
    var el = document.getElementById("c96-rss-topics");
    var cnt = document.getElementById("c96-rss-tcount");
    if (!el) { return; }
    el.textContent = "";
    if (cnt) { cnt.textContent = "(" + list.length + ")"; }
    if (list.length === 0) { el.textContent = "（还没有订阅：上面填关键词点添加）"; return; }
    for (var i = 0; i < list.length; i++) { el.appendChild(rssTopicRow(list[i])); }
  }
  function renderRssSources(rows) {
    var el = document.getElementById("c96-rss-sources");
    var cnt = document.getElementById("c96-rss-scount");
    if (!el) { return; }
    el.textContent = "";
    if (cnt) { cnt.textContent = "(" + rows.length + ")"; }
    if (rows.length === 0) { el.textContent = "（知识库还没有内容）"; return; }
    for (var i = 0; i < rows.length; i++) {
      var r = rows[i];
      var line = document.createElement("div");
      line.setAttribute("style", "display:flex;align-items:center;gap:6px;font-size:12px;padding:2px 0;");
      var nm = document.createElement("span");
      nm.textContent = r[0];
      nm.setAttribute("style", "flex:1;color:#e6e6e6;");
      var ct = document.createElement("span");
      ct.textContent = r[1] + " 条 · " + rssTime(r[2]);
      ct.setAttribute("style", "color:#6b7280;");
      var cp = rssBtnNew(L("copy_url"), null, "#93c5fd");
      cp.addEventListener("click", function (ev) {
        rssCopy(rssUrl(RSS_BASE[0], "/rss/source/" + encodeURIComponent(ev.currentTarget.getAttribute("data-src")) + ".xml", RSS_BASE[2]));
      });
      cp.setAttribute("data-src", r[0]);
      var sclr = rssBtnNew(L("clear"), null, "#f87171");
      sclr.setAttribute("data-src", r[0]);
      sclr.addEventListener("click", function (ev) {
        var b = ev.currentTarget;
        if (b.getAttribute("data-armed") !== "1") { b.setAttribute("data-armed", "1"); b.textContent = L("confirm_clear"); return; }
        var name = b.getAttribute("data-src");
        var raw = (name === "(none)") ? "" : name;
        ipc("source_remove", { source: raw }).then(function (n) {
          rssStatus("已清空来源 " + name + "：删掉 " + n + " 条");
          return refreshLists();
        }).catch(function (e) { rssStatus("清空来源失败：" + e); });
      });
      line.appendChild(nm);
      line.appendChild(ct);
      line.appendChild(cp);
      line.appendChild(sclr);
      el.appendChild(line);
    }
  }
  // ---- 全部内容 / 主机制：逐条删、按站点清空、忽略该站点 ----
  function rssFilter() {
    var el = document.getElementById("c96-rss-dq");
    return el ? (el.value || "") : "";
  }
  function rssOnlyUnsub() {
    var el = document.getElementById("c96-rss-dunsub");
    return !!(el && el.getAttribute("data-on") === "1");
  }
  function rssAnyLine(r) {
    var line = document.createElement("div");
    line.setAttribute("style", "display:flex;align-items:center;gap:6px;font-size:12px;padding:2px 0;");
    var when = document.createElement("span");
    when.textContent = rssTime(r[6]);
    when.setAttribute("style", "color:#6b7280;white-space:nowrap;");
    var a = document.createElement("a");
    a.href = r[1];
    a.textContent = r[2] || r[1];
    a.title = r[1];
    a.setAttribute("style", "flex:1;color:#93c5fd;text-decoration:none;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;");
    var host = document.createElement("span");
    host.textContent = r[3] || "";
    host.setAttribute("style", "color:#6b7280;white-space:nowrap;max-width:150px;overflow:hidden;text-overflow:ellipsis;");
    var tag = document.createElement("span");
    if (r[5]) { tag.textContent = "#" + r[5]; tag.setAttribute("style", "color:#86efac;white-space:nowrap;"); }
    else { tag.textContent = r[4] ? r[4] : "随手"; tag.setAttribute("style", "color:#fbbf24;white-space:nowrap;"); }
    var del = rssBtnNew(L("del"), null, "#f87171");
    del.addEventListener("click", function (ev) {
      var b = ev.currentTarget;
      if (b.getAttribute("data-armed") !== "1") { b.setAttribute("data-armed", "1"); b.textContent = L("confirm"); return; }
      ipc("rss_doc_remove", { hash: r[0] }).then(function (ok) {
        line.style.display = "none";
        rssStatus(ok ? "已删 " + (r[2] || r[0]) : "条目不存在（可能已被 TTL 清掉）");
        if (ok) { return refreshLists(); }
      }).catch(function (e) { rssStatus("删条目失败：" + e); });
    });
    line.appendChild(when);
    line.appendChild(a);
    line.appendChild(host);
    line.appendChild(tag);
    line.appendChild(del);
    return line;
  }
  function renderRssDocs(o) {
    var el = document.getElementById("c96-rss-docs");
    var cnt = document.getElementById("c96-rss-dcount");
    if (!el) { return; }
    var rows = (o && o.rows) ? o.rows : [];
    var total = (o && o.total !== undefined) ? o.total : 0;
    el.textContent = "";
    var mode = rssOnlyUnsub() ? " · 只看随手" : "";
    if (cnt) { cnt.textContent = "(库中 " + total + " 条 · 显示 " + rows.length + " 条" + mode + ")"; }
    if (rows.length === 0) { el.textContent = "（没有匹配内容：换个过滤词，或点「显示全部」）"; return; }
    for (var i = 0; i < rows.length; i++) { el.appendChild(rssAnyLine(rows[i])); }
  }
  function reloadDocs() {
    var el = document.getElementById("c96-rss-docs");
    if (!el) { return Promise.resolve(); }
    el.textContent = L("loading");
    return ipc("docs", { filter: rssFilter(), only_unsub: rssOnlyUnsub(), limit: 300 })
      .then(function (o) { renderRssDocs(o); })
      .catch(function (e) { el.textContent = "读取失败：" + e; });
  }
  function rssHostRow(r) {
    var raw = (r[0] === "(none)") ? "" : r[0];
    var line = document.createElement("div");
    line.setAttribute("style", "display:flex;align-items:center;gap:6px;font-size:12px;padding:2px 0;");
    var nm = document.createElement("span");
    nm.textContent = r[0];
    nm.setAttribute("style", "flex:1;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;color:" + (r[4] ? "#6b7280" : "#e6e6e6") + ";" + (r[4] ? "text-decoration:line-through;" : ""));
    var ct = document.createElement("span");
    ct.textContent = r[1] + " 条 · " + rssTime(r[2]);
    ct.setAttribute("style", "color:#6b7280;white-space:nowrap;");
    line.appendChild(nm);
    line.appendChild(ct);
    if (raw) {
      var clr = rssBtnNew(L("clear"), null, "#f87171");
      clr.addEventListener("click", function (ev) {
        var b = ev.currentTarget;
        if (b.getAttribute("data-armed") !== "1") { b.setAttribute("data-armed", "1"); b.textContent = L("confirm_clear"); return; }
        ipc("host_remove", { host: raw }).then(function (n) {
          rssStatus("已清空 " + raw + "：删掉 " + n + " 条");
          return refreshLists();
        }).catch(function (e) { rssStatus("清空失败：" + e); });
      });
      var ig = rssBtnNew(r[4] ? L("unignore") : L("ignore"), null, r[4] ? "#86efac" : "#fbbf24");
      ig.addEventListener("click", function () {
        ipc("ignore", { host: raw, on: !r[4] }).then(function () {
          rssStatus((r[4] ? "已取消忽略 " : "已忽略 ") + raw + (r[4] ? "" : "（以后不再自动收录）"));
          return refreshLists();
        }).catch(function (e) { rssStatus("忽略失败：" + e); });
      });
      line.appendChild(clr);
      line.appendChild(ig);
    }
    return line;
  }
  function renderRssHosts(rows) {
    var el = document.getElementById("c96-rss-hosts");
    var cnt = document.getElementById("c96-rss-hcount");
    if (!el) { return; }
    el.textContent = "";
    if (cnt) { cnt.textContent = "(" + rows.length + ")"; }
    if (!rows || rows.length === 0) { el.textContent = "（知识库还没有内容）"; return; }
    for (var i = 0; i < rows.length; i++) { el.appendChild(rssHostRow(rows[i])); }
  }
  function reloadHosts() {
    return ipc("hosts").then(function (rows) { renderRssHosts(rows || []); }).catch(function () {});
  }
  // 黑名单：内置规则只读、用户自加规则可删；命中该 glob 的页面不再自动收录
  function rssPatternRow(pat, editable) {
    var line = document.createElement("div");
    line.setAttribute("style", "display:flex;align-items:center;gap:6px;font-size:12px;padding:2px 0;");
    var nm = document.createElement("span");
    nm.textContent = pat;
    nm.setAttribute("style", "flex:1;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;font-family:monospace;color:" + (editable ? "#e6e6e6" : "#6b7280") + ";");
    line.appendChild(nm);
    if (editable) {
      var del = rssBtnNew(L("delete"), null, "#f87171");
      del.addEventListener("click", function (ev) {
        var b = ev.currentTarget;
        if (b.getAttribute("data-armed") !== "1") { b.setAttribute("data-armed", "1"); b.textContent = L("confirm_delete"); return; }
        ipc("pattern_remove", { pattern: pat }).then(function (ok) {
          rssStatus(ok ? "已删除规则：" + pat : "规则不存在（内置规则不可删）");
          return reloadPatterns();
        }).catch(function (e) { rssStatus("删除规则失败：" + e); });
      });
      line.appendChild(del);
    } else {
      var tag = document.createElement("span");
      tag.textContent = L("builtin");
      tag.setAttribute("style", "color:#6b7280;white-space:nowrap;");
      line.appendChild(tag);
    }
    return line;
  }
  function renderPatterns(info) {
    var el = document.getElementById("c96-rss-block");
    var cnt = document.getElementById("c96-rss-bcount");
    if (!el) { return; }
    var built = (info && info.built_in) ? info.built_in : [];
    var user = (info && info.user) ? info.user : [];
    el.textContent = "";
    if (cnt) { cnt.textContent = "(内置 " + built.length + " · 用户 " + user.length + ")"; }
    for (var i = 0; i < user.length; i++) { el.appendChild(rssPatternRow(user[i], true)); }
    for (var j = 0; j < built.length; j++) { el.appendChild(rssPatternRow(built[j], false)); }
  }
  function reloadPatterns() {
    return ipc("patterns").then(function (info) { renderPatterns(info || {}); }).catch(function () {});
  }
  function reloadSources() {
    return ipc("rss_sources").then(function (rows) { renderRssSources(rows || []); }).catch(function () {});
  }
  // 删/清空/忽略后只刷列表（不动关键词订阅，避免把已展开的条目收起来）
  function refreshLists() {
    return Promise.all([reloadSources(), reloadHosts(), reloadDocs(), reloadPatterns()]).then(function () {});
  }
  function renderRssUrls() {
    var el = document.getElementById("c96-rss-urls");
    if (!el) { return; }
    el.textContent = "";
    var local = RSS_BASE[0];
    var lan = RSS_BASE[1];
    var sync = RSS_BASE[2];
    if (!local) { el.textContent = "RSS 服务未启动（GUI 启动时会自动开）"; return; }
    var rows = [
      ["本机", rssUrl(local, "/rss/all.xml", sync), "全部内容"],
      ["本机", rssUrl(local, "/opml.xml", sync), "OPML 一键导入"]
    ];
    if (lan && lan !== local) {
      rows.push(["局域网", rssUrl(lan, "/rss/all.xml", sync), "手机/其他设备"]);
      rows.push(["局域网", rssUrl(lan, "/opml.xml", sync), "OPML"]);
    }
    for (var i = 0; i < rows.length; i++) {
      var row = rows[i];
      var line = document.createElement("div");
      line.setAttribute("style", "display:flex;align-items:center;gap:6px;padding:1px 0;");
      var tag = document.createElement("span");
      tag.textContent = row[0];
      tag.setAttribute("style", "color:#f59e0b;white-space:nowrap;");
      var u = document.createElement("span");
      u.textContent = row[1];
      u.setAttribute("style", "flex:1;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;color:#e6e6e6;");
      var note = document.createElement("span");
      note.textContent = row[2];
      note.setAttribute("style", "color:#6b7280;white-space:nowrap;");
      var cp = rssBtnNew(L("copy"), null, "#93c5fd");
      cp.setAttribute("data-url", row[1]);
      cp.addEventListener("click", function (ev) { rssCopy(ev.currentTarget.getAttribute("data-url")); });
      line.appendChild(tag);
      line.appendChild(u);
      line.appendChild(note);
      line.appendChild(cp);
      el.appendChild(line);
    }
  }
  function reloadRss() {
    rssStatus(L("loading"));
    return ipc("rss_base").then(function (b) {
      RSS_BASE = b || ["", "", ""];
      renderRssUrls();
      return Promise.all([
        ipc("topic_list").catch(function (e) { return { err: String(e) }; }),
        ipc("rss_sources").catch(function () { return []; }),
        ipc("hosts").catch(function () { return []; }),
        ipc("docs", { filter: rssFilter(), only_unsub: rssOnlyUnsub(), limit: 300 }).catch(function (e) { return { err: String(e) }; }),
        ipc("patterns").catch(function () { return { built_in: [], user: [] }; })
      ]);
    }).then(function (res) {
      var ts = (res[0] && res[0].length !== undefined) ? res[0] : [];
      renderRssTopics(ts);
      renderRssSources(res[1] || []);
      renderRssHosts(res[2] || []);
      renderRssDocs(res[3] || {});
      renderPatterns(res[4] || {});
      var bad = (res[0] && res[0].err) ? " · IPC 错误: " + res[0].err : "";
      var shown = (res[3] && res[3].shown !== undefined) ? res[3].shown : 0;
      var bl = ((res[4] && res[4].user) ? res[4].user.length : 0);
      rssStatus("订阅 " + ts.length + " 个 · 来源 " + ((res[1] || []).length) + " 个 · 主机 " + ((res[2] || []).length) + " 个 · 内容 " + shown + " 条 · 用户黑名单 " + bl + " 条" + bad);
    }).catch(function (e) { rssStatus("加载失败：" + e); });
  }
  function toggleRssPanel() {
    if (MODE === "toolbar") { post("ui", "panel"); return; }
    installRssPanel();
    var p = document.getElementById("c96-rss-panel");
    if (!p) { return; }
    if (!p.style.display || p.style.display === "none") {
      p.style.display = "block";
      reloadRss();
    } else {
      p.style.display = "none";
    }
  }
  window.__C96_RSS_UI__ = {
    open: function () {
      installRssPanel();
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "block"; }
      return reloadRss();
    },
    close: function () {
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "none"; }
    },
    reload: function () { return reloadRss(); },
    reloadDocs: function () { return reloadDocs(); },
    reloadHosts: function () { return reloadHosts(); },
    reloadSources: function () { return reloadSources(); },
    status: function (m) { rssStatus(m); },
    reply: rssReply,
    post: post
  };
  function setShellBg(opaque) {
    var bg = opaque ? "#14161a" : "transparent";
    if (document.documentElement) { document.documentElement.style.background = bg; }
    if (document.body) { document.body.style.background = bg; }
  }
  function setShellClick(on) {
    if (document.body) { document.body.style.pointerEvents = on ? "auto" : "none"; }
  }
  function maskShow(text) {
    var m = document.getElementById("c96-mask");
    if (!m) {
      m = document.createElement("div");
      m.id = "c96-mask";
      m.setAttribute("style", "position:fixed;top:0;left:0;right:0;bottom:0;z-index:2147483646;background:rgba(20,22,26,.9);color:#f0f4f8;font:15px/1.6 system-ui,'Segoe UI',sans-serif;display:flex;align-items:center;justify-content:center;");
      var t = document.createElement("div");
      t.id = "c96-mask-text";
      t.setAttribute("style", "padding:14px 18px;background:rgba(28,32,39,.96);border:1px solid #3a4150;border-radius:12px;box-shadow:0 12px 34px rgba(0,0,0,.5);max-width:80%;text-align:center;");
      m.appendChild(t);
      (document.body || document.documentElement).appendChild(m);
    }
    setShellBg(true);
    setShellClick(true);
    m.style.display = "flex";
    var tx = document.getElementById("c96-mask-text");
    if (tx) { tx.textContent = text || ""; }
  }
  function maskHide() {
    var m = document.getElementById("c96-mask");
    if (m) { m.style.display = "none"; }
    setShellBg(false);
    setShellClick(false);
  }
  window.__C96_UI__ = {
    panel: function () {
      maskHide();
      var d = document.getElementById("c96-settings");
      if (d) { d.style.display = "none"; }
      installRssPanel();
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "block"; p.style.maxHeight = "calc(100vh - 74px)"; p.style.pointerEvents = "auto"; }
      return reloadRss();
    },
    settings: function () {
      maskHide();
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "none"; }
      var d = openSettings();
      if (d) { d.style.display = "flex"; d.style.pointerEvents = "auto"; }
      setShellClick(true);
    },
    mask: function (t) {
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "none"; }
      var d = document.getElementById("c96-settings");
      if (d) { d.style.display = "none"; }
      maskShow(t);
    },
    hide: function () {
      maskHide();
      var p = document.getElementById("c96-rss-panel");
      if (p) { p.style.display = "none"; }
      var d = document.getElementById("c96-settings");
      if (d) { d.style.display = "none"; }
    }
  };
  function boot() {
    if (MODE === "toolbar") {
      document.documentElement.style.background = "#14161a";
      if (document.body) { document.body.style.background = "#14161a"; }
      installBar();
      window.__C96_STATUS__ = function (m) {
        var el = document.getElementById("c96-status");
        if (el) { el.textContent = m; }
      };
      window.__C96_SET_LANG__ = function (l) {
        UI_LANG = (l === "en") ? "en" : "zh";
        applyLang();
      };
      syncLang();
      setTimeout(syncLang, 1200);
    } else if (MODE === "overlay") {
      setShellBg(false);
      setShellClick(false);
      syncLang();
      setTimeout(syncLang, 1200);
    } else if (MODE === "content") {
      window.__C96_CAPTURE__ = capture;
      try {
        if (document.body && !document.body.style.paddingTop) { document.body.style.paddingTop = "46px"; }
      } catch (e) {}
      beginCapture();
      setInterval(watchNav, 1500);
    }
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
  post("ready", document.title);
})();"##;


/// 打开（或复用）自控窗口并导航到 url。
pub fn open(app: &AppHandle, url: &str) -> Result<String, String> {
    let parsed = tauri::Url::parse(url).map_err(|e| format!("invalid url: {}", e))?;
    if let Some(win) = app.get_webview_window("browser") {
        let quoted = serde_json::to_string(url).map_err(|e| e.to_string())?;
        let js = "window.location.href = ".to_string() + &quoted + ";";
        win.eval(&js).map_err(|e| e.to_string())?;
        return Ok("reused browser window -> ".to_string() + url);
    }
    let nav_app = app.clone();
    let load_app = app.clone();
    let win = WebviewWindowBuilder::new(app, "browser", WebviewUrl::External(parsed))
        .title("C96 自控窗口 — WebView2")
        .inner_size(1100.0, 800.0)
        .additional_browser_args(BROWSER_ARGS)
        .initialization_script(LINKS_INIT)
        .initialization_script(SNAPSHOT_INIT)
        .initialization_script(EXTRACT_INIT)
        .initialization_script(EVENTS_INIT)
        .on_navigation(move |u| {
            let url = u.to_string();
            println!("[c96 browser] navigate -> {}", url);
            record(&nav_app, "navigation", &url, "");
            true
        })
        .on_page_load(move |_win, payload| {
            let url = payload.url().to_string();
            let kind = match payload.event() {
                tauri::webview::PageLoadEvent::Started => "load-start",
                tauri::webview::PageLoadEvent::Finished => "load-finish",
            };
            record(&load_app, kind, &url, "");
        })
        .build()
        .map_err(|e| e.to_string())?;
    let current = win.url().map(|u| u.to_string()).unwrap_or_default();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let status = match install_web_message(app, "browser") {
        Ok(s) => s,
        Err(e) => "install failed: ".to_string() + &e,
    };
    return Ok("opened browser window -> ".to_string() + &current + " | " + &status);
}
/// E3：主窗口 = 浏览器（外部 URL）+ C96 叠加搜索条 + 自动 RAG 采集。
/// `visible=false`（静默模式）：创建即隐藏窗口，后台 RSS / 调度照常跑，
/// 之后由单实例回调（非静默再次调用）唤出。
pub fn open_main(app: &AppHandle, start_url: &str, visible: bool, small: bool) -> Result<String, String> {
    let parsed = tauri::Url::parse(start_url).map_err(|e| format!("invalid url: {}", e))?;
    let nav_app = app.clone();
    let load_app = app.clone();
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(parsed))
        .title("C96 — 浏览器 / RAG 采集")
        .inner_size(if small { 760.0 } else { 1280.0 }, if small { 540.0 } else { 860.0 })
        .visible(visible)
        .additional_browser_args(BROWSER_ARGS)
        .initialization_script("window.__C96_MODE__='content';")
        .initialization_script(LINKS_INIT)
        .initialization_script(SNAPSHOT_INIT)
        .initialization_script(EXTRACT_INIT)
        .initialization_script(OVERLAY_INIT)
        .on_navigation(move |u| {
            let url = u.to_string();
            record(&nav_app, "navigation", &url, "");
            true
        })
        .on_page_load(move |_win, payload| {
            let url = payload.url().to_string();
            let kind = match payload.event() {
                tauri::webview::PageLoadEvent::Started => "load-start",
                tauri::webview::PageLoadEvent::Finished => "load-finish",
            };
            record(&load_app, kind, &url, "");
        })
        .build()
        .map_err(|e| e.to_string())?;
    let current = win.url().map(|u| u.to_string()).unwrap_or_default();
    // 原生多 WebView：内容页在下，覆盖层居中，顶栏最上。
    // 顶栏/覆盖层是独立 WebView，目标页导航不会刷新它们。
    let win_handle = app.get_window("main").ok_or_else(|| "no main window".to_string())?;
    let scale = win_handle.scale_factor().unwrap_or(1.0);
    let wsize = win_handle.inner_size().map_err(|e| e.to_string())?;
    let lw = wsize.width as f64 / scale;
    let lh = wsize.height as f64 / scale;
    let overlay = win_handle
        .add_child(
            tauri::webview::WebviewBuilder::new("overlay", WebviewUrl::App("blank.html".into()))
                .initialization_script("window.__C96_MODE__='overlay';")
                .initialization_script(OVERLAY_INIT)
                .additional_browser_args(BROWSER_ARGS)
                .transparent(true)
                .auto_resize()
                .focused(false),
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(lw, lh),
        )
        .map_err(|e| e.to_string())?;
    let _ = overlay.hide();
    let toolbar = win_handle
        .add_child(
            tauri::webview::WebviewBuilder::new("toolbar", WebviewUrl::App("blank.html".into()))
                .initialization_script("window.__C96_MODE__='toolbar';")
                .initialization_script(OVERLAY_INIT)
                .additional_browser_args(BROWSER_ARGS)
                .focused(false),
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(lw, 46.0),
        )
        .map_err(|e| e.to_string())?;
    let toolbar2 = toolbar.clone();
    let win2 = win_handle.clone();
    win_handle.on_window_event(move |ev| {
        if let WindowEvent::Resized(size) = ev {
            let scale = win2.scale_factor().unwrap_or(1.0);
            let _ = toolbar2.set_size(PhysicalSize::new(size.width, (46.0 * scale).round() as u32));
            let _ = toolbar2.set_position(PhysicalPosition::new(0, 0));
        }
    });
    let _ = install_web_message(app, "overlay");
    let _ = install_web_message(app, "toolbar");
    return Ok("opened main browser -> ".to_string() + &current);
}

/// 当前事件表最后一条的 seq（爬取时用于区分"本页新产生"的入库事件）。
pub fn last_event_seq() -> u64 {
    let v = events().lock().unwrap_or_else(|e| e.into_inner());
    return v.last().map(|e| e.seq).unwrap_or(0);
}

fn host_of(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    let h = s.split('/').next().unwrap_or(s);
    return h.to_lowercase();
}

/// 浏览器逐页爬取：串行把候选导航进主窗口，等每页自动抽取入库事件出现再下一页。
/// 窗口在屏内可见才渲染（静默时窗口很小）。
pub struct CrawlReport {
    pub stored: u64,
    pub skipped: u64,
    pub failed: u64,
}

pub fn crawl(app: &AppHandle, urls: &[String], query: &str) -> CrawlReport {
    set_query(query);
    ui_mask(app, "自动操作中，请勿操作控件，会导致中断");
    let n = urls.len();
    let mut rep = CrawlReport { stored: 0, skipped: 0, failed: 0 };
    for (i, url) in urls.iter().enumerate() {
        let host = host_of(url);
        ui_mask(app, &format!("自动操作中 {}/{}：{} · 请勿操作控件，会导致中断", i + 1, n, host));
        let base = last_event_seq();
        let _ = raw_navigate(app, "main", url);
        let t0 = std::time::Instant::now();
        let mut done = false;
        while t0.elapsed() < std::time::Duration::from_secs(25) {
            std::thread::sleep(std::time::Duration::from_millis(600));
            for e in list_events().iter().rev() {
                if e.seq > base && e.kind.starts_with("store") && e.url.to_lowercase().contains(&host) {
                    if e.kind == "store" { rep.stored += 1; }
                    else if e.kind == "store-skip" { rep.skipped += 1; }
                    else { rep.failed += 1; }
                    done = true;
                    break;
                }
            }
            if done { break; }
        }
        if !done { rep.failed += 1; }
    }
    ui_mask_hide(app);
    return rep;
}

/// E1 探针：报告自控窗口是否已开、当前 URL、注入的钩子名。
pub fn probe(app: &AppHandle) -> Result<String, String> {
    match app.get_webview_window("browser") {
        Some(win) => {
            let url = win.url().map(|u| u.to_string()).unwrap_or_default();
            let mut s = "browser window: OPEN".to_string();
            s.push(char::from(10));
            s.push_str("current url  : ");
            s.push_str(&url);
            s.push(char::from(10));
            s.push_str("snapshot hook: window.__C96_SNAPSHOT__.capture()");
            s.push(char::from(10));
            s.push_str("event hook   : window.__C96_EVENTS__ (click/spa -> postMessage)");
            return Ok(s);
        }
        None => { return Ok("browser window: CLOSED (click 打开自控窗口)".to_string()); }
    }
}

/// 把闭包放到 webview 主线程执行并取回字符串结果。
/// with_webview 的闭包必须 Send + 'static，所以用 channel 回传。
fn run_on_webview<F>(wv: &tauri::Webview, f: F) -> Result<String, String>
where
    F: FnOnce(&tauri::webview::PlatformWebview) -> String + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    wv.with_webview(move |pw| {
        let _ = tx.send(f(&pw));
    })
    .map_err(|e| e.to_string())?;
    return rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("with_webview timeout: {}", e));
}

fn webview_of(app: &AppHandle, label: &str) -> Result<tauri::Webview, String> {
    return app.get_webview(label).ok_or_else(|| format!("no webview {:?}", label));
}

/// 直接读底层 WebView2 状态（webview2-rs 路径）。
pub fn raw_info(app: &AppHandle, label: &str) -> Result<String, String> {
    let wv = webview_of(app, label)?;
    return run_on_webview(&wv, |pw| raw::info(pw));
}

/// 直接改 WebView2 缩放（webview2-rs 路径）。
pub fn raw_zoom(app: &AppHandle, label: &str, factor: f64) -> Result<String, String> {
    let wv = webview_of(app, label)?;
    return run_on_webview(&wv, move |pw| raw::zoom(pw, factor));
}

/// 直接用 ICoreWebView2::Navigate 导航（webview2-rs 路径）。
pub fn raw_navigate(app: &AppHandle, label: &str, url: &str) -> Result<String, String> {
    let wv = webview_of(app, label)?;
    let url = url.to_string();
    return run_on_webview(&wv, move |pw| raw::navigate(pw, &url));
}

/// 在目标窗口执行 JS 并回传结果字符串（channel 回传，可跨线程调用）。
pub fn eval_string(app: &AppHandle, label: &str, js: &str) -> Result<String, String> {
    let wv = webview_of(app, label)?;
    let js = js.to_string();
    return run_on_webview(&wv, move |pw| raw::eval(pw, &js));
}

/// E2：安装 AddWebMessageReceived（webview2-rs 路径）。
pub fn install_web_message(app: &AppHandle, label: &str) -> Result<String, String> {
    let wv = webview_of(app, label)?;
    let app2 = app.clone();
    let label2 = label.to_string();
    return run_on_webview(&wv, move |pw| install_dispatch(pw, app2, label2));
}

#[cfg(windows)]
fn install_dispatch(pw: &tauri::webview::PlatformWebview, app: AppHandle, label: String) -> String {
    return raw_events::install(pw, app, label);
}

#[cfg(not(windows))]
fn install_dispatch(_pw: &tauri::webview::PlatformWebview, _app: AppHandle, _label: String) -> String {
    return "web message: Windows only".to_string();
}

/// 裸 WebView2 控制：Windows 走 webview2-com，其他平台返回说明字符串。
/// cfg 只放在 item 上（Rust 稳定版不允许把 cfg 属性放在块表达式上）。
mod raw {
    use tauri::webview::PlatformWebview;
    use windows::core::implement;

    #[cfg(windows)]
    pub fn info(pw: &PlatformWebview) -> String {
        return info_win(pw);
    }

    #[cfg(not(windows))]
    pub fn info(_pw: &PlatformWebview) -> String {
        return "raw WebView2: only available on Windows".to_string();
    }

    #[cfg(windows)]
    pub fn zoom(pw: &PlatformWebview, factor: f64) -> String {
        return zoom_win(pw, factor);
    }

    #[cfg(not(windows))]
    pub fn zoom(_pw: &PlatformWebview, _factor: f64) -> String {
        return "raw WebView2: only available on Windows".to_string();
    }

    #[cfg(windows)]
    pub fn navigate(pw: &PlatformWebview, url: &str) -> String {
        return navigate_win(pw, url);
    }

    #[cfg(not(windows))]
    pub fn navigate(_pw: &PlatformWebview, _url: &str) -> String {
        return "raw WebView2: only available on Windows".to_string();
    }

    #[cfg(windows)]
    pub fn eval(pw: &PlatformWebview, js: &str) -> String {
        return eval_win(pw, js);
    }

    #[cfg(not(windows))]
    pub fn eval(_pw: &PlatformWebview, _js: &str) -> String {
        return "raw eval: only available on Windows".to_string();
    }

    #[cfg(windows)]
    fn read_pwstr(p: windows::core::PWSTR) -> String {
        // WebView2 用 CoTaskMemAlloc 分配这些字符串，读完必须释放。
        let s = unsafe { p.to_string().unwrap_or_default() };
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        }
        return s;
    }

    #[cfg(windows)]
    fn info_win(pw: &PlatformWebview) -> String {
        use windows::core::BOOL;
        use windows::core::PWSTR;
        let mut lines: Vec<String> = Vec::new();
        let controller = pw.controller();
        lines.push("PlatformWebview::controller() -> ICoreWebView2Controller  [webview2-com]".to_string());
        unsafe {
            match controller.CoreWebView2() {
                Ok(core) => {
                    lines.push("controller.CoreWebView2() -> ICoreWebView2".to_string());
                    let mut src = PWSTR::null();
                    if core.Source(&mut src).is_ok() {
                        lines.push("Source()        = ".to_string() + &read_pwstr(src));
                    }
                    let mut title = PWSTR::null();
                    if core.DocumentTitle(&mut title).is_ok() {
                        lines.push("DocumentTitle() = ".to_string() + &read_pwstr(title));
                    }
                    if let Ok(s) = core.Settings() {
                        let mut b = BOOL(0);
                        if s.IsScriptEnabled(&mut b).is_ok() {
                            lines.push(format!("Settings.IsScriptEnabled     = {}", b.as_bool()));
                        }
                        let mut b2 = BOOL(0);
                        if s.IsWebMessageEnabled(&mut b2).is_ok() {
                            lines.push(format!("Settings.IsWebMessageEnabled = {}", b2.as_bool()));
                        }
                        let mut b3 = BOOL(0);
                        if s.AreDevToolsEnabled(&mut b3).is_ok() {
                            lines.push(format!("Settings.AreDevToolsEnabled  = {}", b3.as_bool()));
                        }
                    }
                }
                Err(e) => {
                    lines.push("controller.CoreWebView2() failed: ".to_string() + &e.message().to_string());
                }
            }
            let mut rect = windows::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if controller.Bounds(&mut rect).is_ok() {
                lines.push(format!(
                    "controller.Bounds = ({}, {})  {} x {}",
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top
                ));
            }
        }
        let mut s = String::new();
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                s.push(char::from(10));
            }
            s.push_str(l);
        }
        return s;
    }

    #[cfg(windows)]
    fn zoom_win(pw: &PlatformWebview, factor: f64) -> String {
        let controller = pw.controller();
        let r = unsafe { controller.SetZoomFactor(factor) };
        return match r {
            Ok(()) => format!("SetZoomFactor({}) ok", factor),
            Err(e) => "SetZoomFactor failed: ".to_string() + &e.message().to_string(),
        };
    }

    #[cfg(windows)]
    fn navigate_win(pw: &PlatformWebview, url: &str) -> String {
        let controller = pw.controller();
        let core = match unsafe { controller.CoreWebView2() } {
            Ok(c) => c,
            Err(e) => {
                return "CoreWebView2() failed: ".to_string() + &e.message().to_string();
            }
        };
        let target = windows::core::HSTRING::from(url);
        let r = unsafe { core.Navigate(&target) };
        return match r {
            Ok(()) => "ICoreWebView2::Navigate -> ".to_string() + url,
            Err(e) => "Navigate failed: ".to_string() + &e.message().to_string(),
        };
    }

    static EVAL_RES: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    #[cfg(windows)]
    fn eval_res() -> &'static std::sync::Mutex<Option<String>> {
        return EVAL_RES.get_or_init(|| std::sync::Mutex::new(None));
    }

    /// 在本 UI 线程泵一次消息（ExecuteScript 回调靠消息循环派发）。
    #[cfg(windows)]
    fn pump() {
        use windows::Win32::UI::WindowsAndMessaging::DispatchMessageW;
        use windows::Win32::UI::WindowsAndMessaging::PeekMessageW;
        use windows::Win32::UI::WindowsAndMessaging::PM_REMOVE;
        use windows::Win32::UI::WindowsAndMessaging::TranslateMessage;
        use windows::Win32::UI::WindowsAndMessaging::MSG;
        let mut m = MSG::default();
        unsafe {
            while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
    }

    #[cfg(windows)]
    #[implement(webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ExecuteScriptCompletedHandler)]
    struct EvalHandler;

    #[cfg(windows)]
    impl webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ExecuteScriptCompletedHandler_Impl for EvalHandler_Impl {
        fn Invoke(
            &self,
            _errorcode: windows::core::HRESULT,
            result: &windows::core::PCWSTR,
        ) -> windows::core::Result<()> {
            let s = unsafe { result.to_string().unwrap_or_default() };
            *eval_res().lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
            Ok(())
        }
    }

    #[cfg(windows)]
    fn eval_win(pw: &PlatformWebview, js: &str) -> String {
        let controller = pw.controller();
        let core = match unsafe { controller.CoreWebView2() } {
            Ok(c) => c,
            Err(e) => {
                return "eval CoreWebView2() failed: ".to_string() + &e.message().to_string();
            }
        };
        *eval_res().lock().unwrap_or_else(|e| e.into_inner()) = None;
        let handler: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ExecuteScriptCompletedHandler =
            EvalHandler.into();
        let _ = unsafe { core.ExecuteScript(&windows::core::HSTRING::from(js), &handler) };
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(8) {
            {
                let g = eval_res().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = g.as_ref() {
                    return s.clone();
                }
            }
            pump();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        return "eval: no result (timeout)".to_string();
    }
}

/// E2：webview2-com 的 AddWebMessageReceived 回调。
#[cfg(windows)]
mod raw_events {
    use std::sync::OnceLock;

    use tauri::webview::PlatformWebview;
    use tauri::AppHandle;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2WebMessageReceivedEventArgs;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2WebMessageReceivedEventHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2WebMessageReceivedEventHandler_Impl;
    use windows::core::implement;
    use windows::core::Ref;
    use windows::core::PWSTR;

    static APP: OnceLock<AppHandle> = OnceLock::new();

    #[implement(ICoreWebView2WebMessageReceivedEventHandler)]
    struct WebMsgHandler {
        label: String,
    }

    fn read_and_free(p: PWSTR) -> String {
        let s = unsafe { p.to_string().unwrap_or_default() };
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        }
        return s;
    }

    impl ICoreWebView2WebMessageReceivedEventHandler_Impl for WebMsgHandler_Impl {
        fn Invoke(
            &self,
            _sender: Ref<'_, ICoreWebView2>,
            args: Ref<'_, ICoreWebView2WebMessageReceivedEventArgs>,
        ) -> windows::core::Result<()> {
            let args = args.ok()?;
            let mut text = String::new();
            unsafe {
                let mut p = PWSTR::null();
                if args.TryGetWebMessageAsString(&mut p).is_ok() {
                    text = read_and_free(p);
                }
                if text.is_empty() {
                    let mut q = PWSTR::null();
                    if args.WebMessageAsJson(&mut q).is_ok() {
                        text = read_and_free(q);
                    }
                }
            }
            if !text.is_empty() {
                if let Some(app) = APP.get() {
                    super::ingest(app, &self.label, &text);
                }
            }
            return Ok(());
        }
    }

    pub fn install(pw: &PlatformWebview, app: AppHandle, label: String) -> String {
        let _ = APP.set(app);
        unsafe {
            let core = match pw.controller().CoreWebView2() {
                Ok(c) => c,
                Err(e) => {
                    return "CoreWebView2 failed: ".to_string() + &e.message().to_string();
                }
            };
            let handler: ICoreWebView2WebMessageReceivedEventHandler = WebMsgHandler { label }.into();
            return match core.add_WebMessageReceived(&handler, std::ptr::null_mut()) {
                Ok(()) => "add_WebMessageReceived installed".to_string(),
                Err(e) => "add_WebMessageReceived failed: ".to_string() + &e.message().to_string(),
            };
        }
    }
}
