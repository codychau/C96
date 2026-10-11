//! 阶段 I3/I4（精简版）：把常驻进程变成**本地 RSS 服务器**。
//!
//! - 输入：主题任务（关键词）+ 已入库文档（RSS 抓取 / SERP / GUI 浏览器）
//! - 输出：RSS 2.0 + OPML（可直接导入阅读器）+ 一个发现用 HTML 首页
//! - "推送"语义：调度线程按 interval 刷新关键词，**新入库的文档 = 新 <item>**（guid = 内容 hash，
//!   pubDate = stored_at_ms），阅读器轮询就能看到新条目
//!
//! 零依赖：std::net::TcpListener + 每连接一线程；只服务本机（默认 127.0.0.1:8787），无 TLS。

use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::store::doc::now_ms;
use crate::store::kb::make_snippet;
use crate::store::kb::Store;
use crate::store::doc::KbDoc;
use crate::store::state;
use crate::store::state::Topic;

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
const FEED_ITEMS: usize = 50;
const SNIPPET: usize = 240;

// ------------------------------------------------------------------ XML ----

pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    return out;
}

fn days_to_civil(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant civil_from_days
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    return (if m <= 2 { y + 1 } else { y }, m, d);
}

/// RFC 822（RSS pubDate 要求）：Sat, 11 Oct 2026 11:20:03 GMT
pub fn rfc822(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = days_to_civil(days);
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let wd = (days + 4).rem_euclid(7) as usize; // 1970-01-01 = Thursday
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    return format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        names[wd], d, months[(m as usize).saturating_sub(1)], y,
        rem / 3600, (rem % 3600) / 60, rem % 60);
}

/// 主题 URL 短标识（稳定、可放进路径）：FNV-1a 低 48 位。
pub fn topic_slug(key: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    return format!("{:012x}", h & 0xffff_ffff_ffff);
}

fn item_title(d: &KbDoc) -> String {
    if !d.title.trim().is_empty() { return d.title.chars().take(300).collect(); }
    return d.text.chars().take(80).collect();
}

pub fn render_feed(title: &str, subtitle: &str, self_url: &str, docs: &[KbDoc]) -> String {
    let build_ms = docs.iter().map(|d| d.stored_at_ms).max().unwrap_or_else(|| now_ms());
    let mut x = String::with_capacity(4096 + docs.len() * 800);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n<channel>\n");
    x.push_str(&format!("  <title>{}</title>\n", escape_xml(title)));
    x.push_str(&format!("  <link>{}</link>\n", escape_xml(self_url)));
    x.push_str(&format!("  <description>{}</description>\n", escape_xml(subtitle)));
    x.push_str("  <language>zh-cn</language>\n");
    x.push_str(&format!("  <lastBuildDate>{}</lastBuildDate>\n", rfc822(build_ms)));
    x.push_str("  <generator>C96 rss</generator>\n");
    x.push_str(&format!("  <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>\n", escape_xml(self_url)));
    for d in docs {
        x.push_str("  <item>\n");
        x.push_str(&format!("    <title>{}</title>\n", escape_xml(&item_title(d))));
        x.push_str(&format!("    <link>{}</link>\n", escape_xml(&d.url)));
        x.push_str(&format!("    <guid isPermaLink=\"false\">{}</guid>\n", escape_xml(&d.hash)));
        x.push_str(&format!("    <pubDate>{}</pubDate>\n", rfc822(d.stored_at_ms)));
        x.push_str(&format!("    <description>{}</description>\n", escape_xml(&make_snippet(&d.text, "", SNIPPET))));
        x.push_str(&format!("    <category>{}</category>\n", escape_xml(&d.source)));
        x.push_str("  </item>\n");
    }
    x.push_str("</channel>\n</rss>\n");
    return x;
}

/// 主题 feed URL（相对路径，供 index / OPML 复用）。
pub fn topic_path(t: &Topic) -> String {
    return format!("/rss/t/{}.xml", topic_slug(&t.key));
}

/// 给 URL 挂上 token（空 token = 原样返回）。
pub fn tokenize(url: &str, token: &str) -> String {
    if token.is_empty() { return url.to_string(); }
    let sep = if url.contains('?') { '&' } else { '?' };
    return format!("{}{}t={}", url, sep, token);
}

/// 本机在局域网里的 IPv4（UDP connect 技巧，不实际发包；失败则用计算机名，再退 127.0.0.1）。
pub fn lan_ip() -> String {
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = s.local_addr() { return a.ip().to_string(); }
        }
    }
    if let Ok(h) = std::env::var("COMPUTERNAME") { if !h.is_empty() { return h; } }
    return "127.0.0.1".to_string();
}

fn bind_host(bind: &str) -> String {
    let h = match bind.rsplit_once(':') {
        Some((h, _)) => h.to_string(),
        None => bind.to_string(),
    };
    return h.trim_start_matches('[').trim_end_matches(']').to_string();
}

/// 监听 0.0.0.0 时对外应宣传的地址（阅读器要能连上）。
pub fn advertised_host(bind: &str, port: u16) -> String {
    let h = bind_host(bind);
    let wildcard = h.is_empty() || h == "0.0.0.0" || h == "::" || h == "*" || h == "0:0:0:0:0:0:0:0";
    if !wildcard { return format!("{}:{}", h, port); }
    return format!("{}:{}", lan_ip(), port);
}

pub fn advertised(bind: &str) -> String {
    let port: u16 = match bind.rsplit_once(':').and_then(|(_, p)| p.parse().ok()) { Some(p) => p, None => 8787 };
    return advertised_host(bind, port);
}

pub fn is_loopback(bind: &str) -> bool {
    let h = bind_host(bind);
    return h == "127.0.0.1" || h == "localhost" || h == "::1";
}

/// 局域网用的随机 token（非密码学强度，够挡局域网随手访问；用 ASLR/时间/pid 混合）。
pub fn gen_token() -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |v: u64| { h ^= v; h = h.wrapping_mul(0x100000001b3); };
    mix(now_ms());
    mix(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0));
    mix(std::process::id() as u64);
    let stack = 0u8;
    mix(&stack as *const u8 as u64);
    return format!("{:016x}", h);
}

fn check_token(query: &str, token: &str) -> bool {
    if token.is_empty() { return true; }
    return query_param(query, "t") == token || query_param(query, "token") == token;
}

pub fn render_opml(bind: &str, topics: &[Topic], token: &str) -> String {
    let base = format!("http://{}", bind);
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n<head>\n");
    x.push_str("  <title>C96 订阅</title>\n");
    x.push_str(&format!("  <dateCreated>{}</dateCreated>\n", rfc822(now_ms())));
    x.push_str("</head>\n<body>\n");
    x.push_str(&format!("  <outline text=\"C96 全部最新\" title=\"C96 全部最新\" type=\"rss\" xmlUrl=\"{}\"/>\n", escape_xml(&tokenize(&format!("{}/rss/all.xml", base), token))));
    for t in topics {
        x.push_str(&format!("  <outline text=\"{}\" title=\"{}\" type=\"rss\" xmlUrl=\"{}\" htmlUrl=\"https://www.bing.com/search?q={}\"/>\n",
            escape_xml(&t.query), escape_xml(&t.query), escape_xml(&tokenize(&format!("{}{}", base, topic_path(t)), token)), escape_xml(&percent_encode(&t.query))));
    }
    x.push_str("</body>\n</opml>\n");
    return x;
}

// --------------------------------------------------------------- 路由 -----

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    return out;
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            match u8::from_str_radix(hex, 16) {
                Ok(v) => { out.push(v); i += 3; continue; }
                Err(_) => {}
            }
        }
        if c == b'+' { out.push(b' '); } else { out.push(c); }
        i += 1;
    }
    return String::from_utf8_lossy(&out).to_string();
}

fn split_target(target: &str) -> (String, String) {
    match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i + 1..].to_string()),
        None => (target.to_string(), String::new()),
    }
}

fn query_param(query: &str, key: &str) -> String {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        if k == key { return percent_decode(v); }
    }
    return String::new();
}

fn status_text(s: u16) -> &'static str {
    match s {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn docs_for_topic(store: &Store, t: &Topic) -> Vec<KbDoc> {
    let mut out: Vec<KbDoc> = store.list(0, "").into_iter()
        .filter(|d| state::topic_key(&d.query) == t.key)
        .collect();
    out.sort_by(|a, b| b.stored_at_ms.cmp(&a.stored_at_ms));
    out.truncate(FEED_ITEMS);
    return out;
}

fn index_html(store: &Store, bind: &str, token: &str) -> String {
    let base = format!("http://{}", bind);
    let st = store.stats();
    let topics = &store.state().topics;
    let mut h = String::new();
    h.push_str("<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\"><title>C96 RSS</title>");
    h.push_str("<style>body{font-family:system-ui,Segoe UI,Microsoft YaHei,sans-serif;max-width:900px;margin:2rem auto;padding:0 1rem;line-height:1.6}code{background:#f2f2f2;padding:.1rem .3rem;border-radius:3px}li{margin:.25rem 0}</style></head><body>");
    h.push_str("<h1>C96 · RSS 服务</h1>");
    h.push_str(&format!("<p>知识库 <code>{}</code> · {} 文档 · TTL {} 天 · 主题任务 {} 个</p>", escape_xml(&st.path), st.docs, st.ttl_days, topics.len()));
    h.push_str("<h2>订阅地址</h2><ul>");
    let all_url = tokenize(&format!("{}/rss/all.xml", base), token);
    h.push_str(&format!("<li><a href=\"{}\" style=\"word-break:break-all\">{}</a> — 全部最新 {} 条</li>", escape_xml(&all_url), escape_xml(&all_url), st.docs.min(FEED_ITEMS)));
    for t in topics {
        let u = tokenize(&format!("{}{}", base, topic_path(t)), token);
        h.push_str(&format!("<li><a href=\"{}\" style=\"word-break:break-all\">{}</a> — 关键词「{}」 runs={} 间隔 {}s</li>",
            escape_xml(&u), escape_xml(&u), escape_xml(&t.query), t.runs, t.interval_secs));
    }
    h.push_str("</ul>");
    let opml_url = tokenize(&format!("{}/opml.xml", base), token);
    h.push_str(&format!("<p><a href=\"{}\">OPML 一键导入</a> · <a href=\"{}\">/health</a> · token 已生效（地址里带 t=…）</p>", escape_xml(&opml_url), escape_xml(&tokenize(&format!("{}/health", base), token))));
    h.push_str("</body></html>\n");
    return h;
}

fn mime() -> &'static str { return "application/rss+xml; charset=utf-8"; }

/// 纯函数路由：便于单测（不起 socket）。
pub fn handle_request(store: &Store, method: &str, target: &str, bind: &str, token: &str) -> (u16, &'static str, String) {
    if method != "GET" && method != "HEAD" {
        return (405, "text/plain; charset=utf-8", "method not allowed\n".to_string());
    }
    let (path, query) = split_target(target);
    if !check_token(&query, token) {
        return (401, "text/plain; charset=utf-8",
            "401 unauthorized：这个 RSS 服务设了 token，订阅地址必须带 ?t=…（见启动日志，或本机打开 / 看 OPML）。\n".to_string());
    }
    let base = format!("http://{}", bind);
    if path == "/" || path == "/index.html" {
        return (200, "text/html; charset=utf-8", index_html(store, bind, token));
    }
    if path == "/health" {
        return (200, "text/plain; charset=utf-8", "ok\n".to_string());
    }
    if path == "/opml.xml" || path == "/rss/opml.xml" {
        return (200, "text/x-opml; charset=utf-8", render_opml(bind, &store.state().topics, token));
    }
    if path == "/rss/all.xml" {
        let docs = store.list(FEED_ITEMS, "");
        let body = render_feed("C96 · 全部最新", "C96 本地知识库（所有来源）", &tokenize(&format!("{}/rss/all.xml", base), token), &docs);
        return (200, mime(), body);
    }
    if let Some(rest) = path.strip_prefix("/rss/source/") {
        let name = rest.strip_suffix(".xml").unwrap_or(rest);
        if name.is_empty() {
            return (404, "text/plain; charset=utf-8", "404: 空的 source 名\n".to_string());
        }
        let docs = store.list(FEED_ITEMS, name);
        let body = render_feed(&format!("C96 · {}", name), &format!("来源 {}（{} 条）", name, docs.len()),
            &tokenize(&format!("{}/rss/source/{}.xml", base, name), token), &docs);
        return (200, mime(), body);
    }
    // /rss/t/<slug>.xml  或  /rss/topic.xml?q=<关键词>
    let mut topic: Option<Topic> = None;
    if let Some(rest) = path.strip_prefix("/rss/t/") {
        let slug = rest.strip_suffix(".xml").unwrap_or(rest);
        for t in store.state().topics.iter() {
            if topic_slug(&t.key) == slug { topic = Some(t.clone()); break; }
        }
    } else if path == "/rss/topic.xml" || path == "/rss/topic" {
        let q = query_param(&query, "q");
        if !q.is_empty() {
            if let Some(i) = state::find_topic(store.state(), &q) {
                topic = Some(store.state().topics[i].clone());
            }
        }
    }
    if let Some(t) = topic {
        let docs = docs_for_topic(store, &t);
        let body = render_feed(&format!("C96 · {}", t.query),
            &format!("关键词「{}」· 每 {}s 刷新 · 命中 {} 条", t.query, t.interval_secs, docs.len()),
            &tokenize(&format!("{}{}", base, topic_path(&t)), token), &docs);
        return (200, mime(), body);
    }
    return (404, "text/plain; charset=utf-8", format!("404 not found: {}\n\ntry / , /rss/all.xml , /opml.xml\n", path));
}

// --------------------------------------------------------------- 服务 -----

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) { Ok(n) => n, Err(_) => return None };
    if n == 0 { return None; }
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let first = text.lines().next().unwrap_or("");
    let mut it = first.split_whitespace();
    let method = it.next().unwrap_or("GET").to_string();
    let target = it.next().unwrap_or("/").to_string();
    return Some((method, target));
}

fn handle_conn(stream: &mut TcpStream, store: &Arc<Mutex<Store>>, bind: &str, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let (method, target) = match read_request(stream) { Some(v) => v, None => return };
    let (status, ctype, body) = {
        let mut g = match store.lock() { Ok(g) => g, Err(e) => e.into_inner() };
        // 外部进程（fetch --store / research / 另一个 CLI）写库后，这里必须能看见
        if g.reload_if_changed() {
            println!("[c96 rss] kb 有外部写入，已重载（docs={}）", g.stats().docs);
        }
        handle_request(&g, &method, &target, bind, token)
    };
    let detail = if ctype.starts_with("application/rss") {
        format!("{} item(s)", body.matches("<item>").count())
    } else {
        format!("{} bytes", body.len())
    };
    println!("[c96 rss] {} {} -> {} ({})", method, target, status, detail);
    let head = format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        status, status_text(status), ctype, body.len());
    let _ = stream.write_all(head.as_bytes());
    if method != "HEAD" { let _ = stream.write_all(body.as_bytes()); }
    let _ = stream.flush();
}

/// 起服务（后台线程），返回实际绑定地址；GUI / CLI 都用它。
pub fn spawn_server(store: Arc<Mutex<Store>>, bind: &str, token: &str) -> Result<String, String> {
    let listener = TcpListener::bind(bind).map_err(|e| format!("bind {} 失败: {}", bind, e))?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(8787);
    let adv = advertised_host(bind, port);
    let b = adv.clone();
    let tok = token.to_string();
    let bind_str = bind.to_string();
    let masked = if tok.is_empty() { "（未设 token，仅本机用）".to_string() } else { format!("token={}", tok) };
    std::thread::spawn(move || {
        println!("[c96 rss] 监听 {} · 对外地址 http://{}/ （OPML: http://{}/opml.xml）{}", bind_str, b, b, masked);
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => {
                    let st = store.clone();
                    let bb = b.clone();
                    let tt = tok.clone();
                    let _ = s.set_nodelay(true);
                    std::thread::spawn(move || handle_conn(&mut s, &st, &bb, &tt));
                }
                Err(e) => eprintln!("[c96 rss] accept error: {}", e),
            }
        }
    });
    return Ok(adv);
}

#[cfg(test)]
mod tests {
    use super::advertised_host;
    use super::escape_xml;
    use super::gen_token;
    use super::handle_request;
    use super::is_loopback;
    use super::tokenize;
    use super::percent_decode;
    use super::rfc822;
    use super::render_feed;
    use super::render_opml;
    use super::topic_slug;
    use crate::fetch::feed::parse_feed;
    use crate::store::kb::Store;
    use crate::store::state;
    use std::path::PathBuf;

    fn kb(name: &str) -> PathBuf {
        let p = PathBuf::from("_scratch").join(name);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(crate::store::kb::default_state_path(&p));
        return p;
    }

    #[test]
    fn escaping_and_dates() {
        assert_eq!(escape_xml("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&apos;");
        assert_eq!(escape_xml("tab\tline\nok"), "tab\tline\nok");
        assert_eq!(rfc822(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(rfc822(1_700_000_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
        assert_eq!(topic_slug("卡普空最新游戏").len(), 12);
        assert_eq!(topic_slug("卡普空最新游戏"), topic_slug("卡普空最新游戏"));
        assert_ne!(topic_slug("a"), topic_slug("b"));
        assert_eq!(percent_decode("%E5%8D%A1+%E6%99%AE"), "卡 普");
    }

    #[test]
    fn rendered_feed_parses_with_own_parser() {
        let p = kb("f_rss_render.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/1", "标题 & 符号 100% 测试", "正文内容 卡普空 最新 发布了一款新游戏", "卡普空", "research-page", 0).unwrap();
        s.save("https://b.example.com/2", "第二条", "另一篇正文", "卡普空", "http-feed", 0).unwrap();
        let docs = s.list(50, "");
        assert_eq!(docs.len(), 2);
        let xml = render_feed("C96 · 测试", "sub", "http://127.0.0.1:8787/rss/all.xml", &docs);
        assert!(xml.contains("<rss version=\"2.0\""));
        let feed = parse_feed(&xml).expect("自家服务器输出的 RSS 必须能被自家解析器解析");
        assert_eq!(feed.items.len(), 2);
        let a = feed.items.iter().find(|i| i.url.starts_with("https://a.example.com")).expect("item a");
        assert_eq!(a.title, "标题 & 符号 100% 测试", "XML 实体必须能原样解析回来，实际: {:?}", a.title);
        assert!(a.body.contains("卡普空"));
        assert!(!a.published.is_empty(), "pubDate 应被解析");
        assert!(feed.items.iter().any(|i| i.url.starts_with("https://b.example.com")));
        let opml = render_opml("127.0.0.1:8787", &s.state().topics, "");
        assert!(opml.contains("<opml version=\"2.0\">"));
        assert!(opml.contains("/rss/all.xml"));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_rss_render.state.toml");
    }

    #[test]
    fn reload_sees_external_writes() {
        let p = kb("f_rss_reload.jsonl");
        let mut server = Store::open(&p, 30).unwrap();
        assert!(!server.reload_if_changed(), "无人写入时不应重载");
        {
            // 模拟"另一个进程"：独立 Store 实例 append
            let mut other = Store::open(&p, 30).unwrap();
            other.save("https://ext.example.com/1", "外部新增", "外部进程写入的正文 卡普空", "", "manual", 0).unwrap();
        }
        assert!(server.reload_if_changed(), "外部写入后必须重载");
        assert_eq!(server.stats().docs, 1);
        let (st, _, body) = handle_request(&server, "GET", "/rss/all.xml", "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        assert_eq!(body.matches("<item>").count(), 1);
        assert!(!server.reload_if_changed(), "无新写入时第二次不应重载");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_rss_reload.state.toml");
    }

    #[test]
    fn token_gate_and_lan_advertise() {
        let p = kb("f_rss_token.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/1", "标题", "正文", "", "manual", 0).unwrap();
        // 无 token 配置 = 不设门
        let (st, _, _) = handle_request(&s, "GET", "/rss/all.xml", "192.168.1.5:8787", "");
        assert_eq!(st, 200);
        // 设了 token：裸访问 401，带 ?t= 200，且 feed 自链接带 token
        let (st, _, body) = handle_request(&s, "GET", "/rss/all.xml", "192.168.1.5:8787", "s3cr3t");
        assert_eq!(st, 401);
        assert!(body.contains("401"));
        let (st, _, body) = handle_request(&s, "GET", "/rss/all.xml?t=s3cr3t", "192.168.1.5:8787", "s3cr3t");
        assert_eq!(st, 200);
        assert!(body.contains("http://192.168.1.5:8787/rss/all.xml?t=s3cr3t"));
        assert!(!body.contains("http://192.168.1.5:8787/rss/all.xml<"));
        // ?token= 也行
        let (st, _, _) = handle_request(&s, "GET", "/health?token=s3cr3t", "192.168.1.5:8787", "s3cr3t");
        assert_eq!(st, 200);
        // 首页 / OPML 链接内嵌 token
        let (st, _, html) = handle_request(&s, "GET", "/?t=s3cr3t", "192.168.1.5:8787", "s3cr3t");
        assert_eq!(st, 200);
        assert!(html.contains("/opml.xml?t=s3cr3t"), "{}", html);
        let opml = render_opml("192.168.1.5:8787", &s.state().topics, "s3cr3t");
        assert!(opml.contains("/rss/all.xml?t=s3cr3t"), "{}", opml);
        // 通配监听要宣传真实可达地址
        let adv = advertised_host("0.0.0.0:8787", 8787);
        assert!(adv.ends_with(":8787"), "{}", adv);
        assert!(!adv.starts_with("0.0.0.0"), "{}", adv);
        assert!(is_loopback("127.0.0.1:8787") && is_loopback("localhost:8787") && !is_loopback("0.0.0.0:8787"));
        assert_eq!(tokenize("http://h/a.xml", ""), "http://h/a.xml");
        assert_eq!(tokenize("http://h/a.xml", "T"), "http://h/a.xml?t=T");
        assert_eq!(tokenize("http://h/a.xml?q=1", "T"), "http://h/a.xml?q=1&t=T");
        let t = gen_token();
        assert_eq!(t.len(), 16);
        assert_ne!(t, gen_token());
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_rss_token.state.toml");
    }

    #[test]
    fn routes_and_topic_slug() {
        let p = kb("f_rss_route.jsonl");
        let mut s = Store::open(&p, 30).unwrap();
        s.save("https://a.example.com/1", "卡普空新闻", "卡普空 发布 新作", "卡普空最新游戏", "research-page", 0).unwrap();
        s.save("https://b.example.com/2", "别的", "无关内容", "其他关键词", "http-feed", 0).unwrap();
        let spec = crate::research::TopicSpec::new("卡普空最新游戏");
        let _ = crate::research::ensure_topic(&mut s, &spec, crate::store::doc::now_ms());
        let slug = topic_slug(&s.state().topics[0].key);

        let (st, ct, body) = handle_request(&s, "GET", "/", "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        assert!(ct.starts_with("text/html"));
        assert!(body.contains("/rss/all.xml") && body.contains(&slug));

        let (st, ct, body) = handle_request(&s, "GET", "/rss/all.xml", "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        assert!(ct.starts_with("application/rss"));
        assert_eq!(body.matches("<item>").count(), 2);

        let (st, _, body) = handle_request(&s, "GET", &format!("/rss/t/{}.xml", slug), "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        assert_eq!(body.matches("<item>").count(), 1, "主题 feed 只含该关键词的文档");

        let (st, _, body) = handle_request(&s, "GET", "/rss/topic.xml?q=%E5%8D%A1%E6%99%AE%E7%A9%BA%E6%9C%80%E6%96%B0%E6%B8%B8%E6%88%8F", "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        assert_eq!(body.matches("<item>").count(), 1);

        let (st, _, _) = handle_request(&s, "GET", "/rss/source/http-feed.xml", "127.0.0.1:8787", "");
        assert_eq!(st, 200);
        let (st, _, _) = handle_request(&s, "POST", "/rss/all.xml", "127.0.0.1:8787", "");
        assert_eq!(st, 405);
        let (st, _, _) = handle_request(&s, "GET", "/nope", "127.0.0.1:8787", "");
        assert_eq!(st, 404);
        let _ = state::remove_topic(s.state_mut(), "卡普空最新游戏");
        let _ = s.persist_state();
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_rss_route.state.toml");
    }
}
