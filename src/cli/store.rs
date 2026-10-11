//! F CLI：store 命令组 + 各抓取路径的 --store 入库。
//!
//!   store stats|list|search|purge|add|state
//!   fetch --store / feed parse --store / html extract --store / search --store
//!
//! 两条入库路径写同一个知识库：GUI 浏览器路径在 gui crate 里调用 C96::store，
//! 这里负责 HTTP 客户端路径（RSS/Atom/HTML/纯文本/SERP 摘要）与手工入库。

use std::path::PathBuf;

use crate::cli::args::Parsed;
use crate::fetch::feed::parse_feed;
use crate::fetch::html::extract;
use crate::fetch::http::Reply;
use crate::store::doc::now_ms;
use crate::store::kb::default_kb_path;
use crate::store::kb::Store;
use crate::store::kb::DEFAULT_TTL_DAYS;
use crate::store::state;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// 一条待入库草稿（各抓取路径组装后交给 ingest）。
pub struct Draft {
    pub url: String,
    pub title: String,
    pub text: String,
    pub query: String,
    pub source: String,
    pub min_len: usize,
}

pub fn usage() {
    println!("usage: C96 store <subcommand> [options]");
    println!("  store stats [--kb PATH] [--ttl-days N]         knowledge base summary");
    println!("  store list [--kb PATH] [--limit N] [--source S] newest docs first");
    println!("  store search QUERY [--limit N]                substring search + snippets");
    println!("  store purge [--kb PATH] [--ttl-days N]        drop expired docs (open auto-purges too)");
    println!("  store state [--kb PATH]                       F3 state file (last_fetch/etag/seen_ids/engine)");
    println!("  store add --url URL [--title T] [--text T | --text-file PATH] [--query Q] [--source S]");
    println!("  --kb PATH         default: %APPDATA%/com.c96.gui/kb.jsonl (same file as the GUI)");
    println!("  --ttl-days N      default 30; docs older than this are discarded");
    println!("  --backdate-days N store add: backdate stored_at_ms (TTL verification)");
    println!("  --min-len N       skip bodies shorter than N chars (default 0)");
}

fn num_opt(parsed: &Parsed, name: &str, dflt: u64) -> u64 {
    let raw = parsed.opt_or(name, "");
    if raw.is_empty() { return dflt; }
    match raw.parse::<u64>() {
        Ok(v) => { return v; }
        Err(_) => { println!("  ignoring --{}: not a number: {}", name, raw); return dflt; }
    }
}

fn usize_opt(parsed: &Parsed, name: &str, dflt: usize) -> usize {
    let raw = parsed.opt_or(name, "");
    if raw.is_empty() { return dflt; }
    match raw.parse::<usize>() {
        Ok(v) => { return v; }
        Err(_) => { println!("  ignoring --{}: not a number: {}", name, raw); return dflt; }
    }
}

pub fn kb_path(parsed: &Parsed) -> String {
    let p = parsed.opt_or("kb", "");
    if !p.is_empty() { return p; }
    return default_kb_path().to_string_lossy().to_string();
}

pub fn ttl_days(parsed: &Parsed) -> u64 {
    return num_opt(parsed, "ttl-days", DEFAULT_TTL_DAYS);
}

pub fn open(parsed: &Parsed) -> Result<Store, String> {
    let path = PathBuf::from(kb_path(parsed));
    return Store::open(&path, ttl_days(parsed));
}

fn dash(s: &str) -> String {
    if s.is_empty() { return "-".to_string(); }
    return s.to_string();
}

/// Unix ms -> UTC 日期字符串（无 chrono/jiff 依赖）。
pub fn fmt_ms(ms: u64) -> String {
    let total = ms / 1000;
    let days = (total / 86400) as i64;
    let rem = total % 86400;
    let (y, m, d) = civil_from_days(days);
    return format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60);
}

fn civil_from_days(z0: i64) -> (i64, i64, i64) {
    let z = z0 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if m <= 2 { y + 1 } else { y };
    return (yy, m, d);
}

fn age_days(ms: u64) -> u64 {
    let now = now_ms();
    if now <= ms { return 0; }
    return (now - ms) / DAY_MS;
}

/// 批量入库（同一次打开，逐条 F2 去重）。返回摘要 + 前几条 skip 原因。
pub fn ingest(parsed: &Parsed, drafts: Vec<Draft>) -> Result<String, String> {
    let mut s = open(parsed)?;
    let mut stored = 0usize;
    let mut skipped = 0usize;
    let mut notes: Vec<String> = Vec::new();
    for d in drafts {
        match s.save(&d.url, &d.title, &d.text, &d.query, &d.source, d.min_len) {
            Ok(msg) => {
                if msg.starts_with("stored:") {
                    stored += 1;
                } else {
                    skipped += 1;
                    if notes.len() < 6 { notes.push(msg); }
                }
            }
            Err(e) => { if notes.len() < 6 { notes.push(e); } }
        }
    }
    let st = s.stats();
    let mut out = format!("store: {} stored / {} skipped (kb={}, docs={}, ttl={}d, purged={})",
        stored, skipped, st.path, st.docs, st.ttl_days, st.purged);
    for n in notes { out = out + "\n  " + &n; }
    return Ok(out);
}

/// 有些站点 / 304 响应不带 Content-Type（或误标 text/plain）：按开头标记嗅探 HTML。
fn looks_like_html(body: &str) -> bool {
    let head: String = body.chars().take(2048).collect();
    let low = head.to_lowercase();
    if low.contains("<!doctype html") { return true; }
    if low.contains("<html") { return true; }
    if low.contains("<body") { return true; }
    if low.contains("<meta ") && low.contains("<div") { return true; }
    return false;
}

/// fetch --store：按 content-type 分派 解析/抽取 -> 入库，并写 F3 状态文件。
pub fn ingest_reply(parsed: &Parsed, rep: &Reply, body: &str) -> Result<String, String> {
    let url = if rep.final_url.is_empty() { rep.url.clone() } else { rep.final_url.clone() };
    // 304：内容未变，上一次抓取已经入库（304 的 content-type 不可靠，会把整篇 XML 当纯文本存成一条 blob）
    if rep.not_modified {
        return Ok(format!("unchanged: 304 (not modified) — {} 未变，跳过入库", url));
    }
    let ct = rep.content_type.to_lowercase();
    let mut drafts: Vec<Draft> = Vec::new();
    let mut kind = "text";
    if ct.contains("xml") || ct.contains("rss") || ct.contains("atom") {
        kind = "feed";
        match parse_feed(body) {
            Ok(feed) => {
                for a in &feed.items {
                    drafts.push(Draft {
                        url: a.url.clone(),
                        title: a.title.clone(),
                        text: a.body.clone(),
                        query: "".to_string(),
                        source: "http-feed".to_string(),
                        min_len: 0,
                    });
                }
                if drafts.is_empty() {
                    drafts.push(Draft {
                        url: url.clone(),
                        title: feed.title.clone(),
                        text: body.to_string(),
                        query: "".to_string(),
                        source: "http-feed-raw".to_string(),
                        min_len: 0,
                    });
                }
            }
            Err(e) => { return Err(format!("feed parse failed: {}", e)); }
        }
    } else if ct.contains("html") || looks_like_html(body) {
        kind = "html";
        let ex = extract(body);
        drafts.push(Draft {
            url: url.clone(),
            title: ex.title.clone(),
            text: ex.text.clone(),
            query: "".to_string(),
            source: "http-html".to_string(),
            min_len: 40,
        });
    } else {
        drafts.push(Draft {
            url: url.clone(),
            title: "".to_string(),
            text: body.to_string(),
            query: "".to_string(),
            source: "http-text".to_string(),
            min_len: 40,
        });
    }
    let mut s = open(parsed)?;
    let mut stored = 0usize;
    let mut skipped = 0usize;
    let mut notes: Vec<String> = Vec::new();
    for d in &drafts {
        match s.save(&d.url, &d.title, &d.text, &d.query, &d.source, d.min_len) {
            Ok(msg) => {
                if msg.starts_with("stored:") {
                    stored += 1;
                } else {
                    skipped += 1;
                    if notes.len() < 6 { notes.push(msg); }
                }
            }
            Err(e) => { if notes.len() < 6 { notes.push(e); } }
        }
    }
    s.record_fetch(&url, kind, rep.status as u64, &rep.etag, &rep.last_modified)?;
    let st = s.stats();
    let mut out = format!("store: {} stored / {} skipped (kind={}, kb={}, docs={}, ttl={}d, purged={})",
        stored, skipped, kind, st.path, st.docs, st.ttl_days, st.purged);
    for n in notes { out = out + "\n  " + &n; }
    return Ok(out);
}

fn cmd_stats(parsed: &Parsed) -> i32 {
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store stats: {}", e); return 1; }
    };
    let st = s.stats();
    println!("store: {}", st.path);
    println!("  docs:           {}", st.docs);
    println!("  bytes:          {}", st.bytes);
    println!("  ttl_days:       {}", st.ttl_days);
    println!("  purged (open):  {}", st.purged);
    println!("  state entries:  {} ({})", st.state_entries, s.state_path());
    return 0;
}

fn cmd_list(parsed: &Parsed) -> i32 {
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store list: {}", e); return 1; }
    };
    let st = s.stats();
    let limit = usize_opt(parsed, "limit", 20);
    let source = parsed.opt_or("source", "");
    let rows = s.list(limit, &source);
    println!("store: {} ({} doc(s), ttl={}d, purged(open)={}, filter source={})",
        st.path, st.docs, st.ttl_days, st.purged, dash(&source));
    let mut i = 0usize;
    for d in &rows {
        i += 1;
        let age = age_days(d.stored_at_ms);
        let left = st.ttl_days.saturating_sub(age);
        println!("  {}. [{}] {} (age {}d, ttl left {}d)", i, dash(&d.source), fmt_ms(d.stored_at_ms), age, left);
        println!("     hash: {}  host: {}", d.hash, dash(&d.host));
        println!("     title: {}", dash(&d.title));
        println!("     url:   {}  ({} chars)", d.url, d.text_len);
    }
    if rows.is_empty() { println!("  (no matching docs)"); }
    return 0;
}

fn cmd_search(parsed: &Parsed) -> i32 {
    let q = if parsed.positional.len() > 2 {
        parsed.positional[2].clone()
    } else {
        parsed.opt_or("query", "")
    };
    if q.trim().is_empty() {
        eprintln!("store search: QUERY or --query is required");
        usage();
        return 1;
    }
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store search: {}", e); return 1; }
    };
    let limit = usize_opt(parsed, "limit", 10);
    let hits = s.search(&q, limit);
    println!("store search \"{}\": {} hit(s)", q, hits.len());
    for (i, h) in hits.iter().enumerate() {
        println!("  {}. score={} [{}] {} ({})", i + 1, h.score, dash(&h.source), h.host, fmt_ms(h.stored_at_ms));
        println!("     title:   {}", dash(&h.title));
        println!("     snip:    {}", h.snippet.replace('\n', " "));
        println!("     url:     {}", h.url);
    }
    return 0;
}

fn cmd_purge(parsed: &Parsed) -> i32 {
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store purge: {}", e); return 1; }
    };
    let st = s.stats();
    println!("store purge: dropped {} expired doc(s) on open, {} remain (kb={}, ttl={}d)",
        st.purged, st.docs, st.path, st.ttl_days);
    return 0;
}

fn cmd_add(parsed: &Parsed) -> i32 {
    let url = parsed.opt_or("url", "");
    if url.is_empty() {
        eprintln!("store add: --url URL is required");
        usage();
        return 1;
    }
    let title = parsed.opt_or("title", "");
    let text_file = parsed.opt_or("text-file", "");
    let text = if !text_file.is_empty() {
        match std::fs::read_to_string(&text_file) {
            Ok(t) => t,
            Err(e) => { eprintln!("store add: cannot read {}: {}", text_file, e); return 1; }
        }
    } else {
        parsed.opt_or("text", "")
    };
    let query = parsed.opt_or("query", "");
    let source = {
        let s = parsed.opt_or("source", "");
        if s.is_empty() { "manual".to_string() } else { s }
    };
    let min_len = usize_opt(parsed, "min-len", 0);
    let backdate = num_opt(parsed, "backdate-days", 0);
    let mut s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store add: {}", e); return 1; }
    };
    let res = if backdate > 0 {
        s.add_backdated(&url, &title, &text, &query, &source, min_len, backdate)
    } else {
        s.save(&url, &title, &text, &query, &source, min_len)
    };
    match res {
        Ok(msg) => println!("store add: {}", msg),
        Err(e) => { eprintln!("store add: {}", e); return 1; }
    }
    let st = s.stats();
    println!("  kb: {} doc(s), {} bytes, ttl={}d, purged(open)={}", st.docs, st.bytes, st.ttl_days, st.purged);
    return 0;
}

fn cmd_state(parsed: &Parsed) -> i32 {
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("store state: {}", e); return 1; }
    };
    let stt = s.state();
    println!("store state: {} (F3)", s.state_path());
    println!("  version={} ttl_days={} last_purge_ms={} entries={}",
        stt.version, stt.ttl_days, stt.last_purge_ms, stt.entries.len());
    for e in &stt.entries {
        println!("  [{}] status={} etag={} last_fetch={}", dash(&e.engine), e.status, dash(&e.etag), fmt_ms(e.last_fetch_ms));
        println!("     url: {}  hash: {}", e.url, dash(&e.kb_hash));
    }
    let ids = state::seen_ids(stt);
    let shown: Vec<String> = ids.iter().take(8).cloned().collect();
    println!("  seen_ids: {}{}", shown.len(),
        if ids.is_empty() { " (-)".to_string() } else { format!(" [{}]", shown.join(", ")) });
    return 0;
}

pub fn run(parsed: &Parsed) -> i32 {
    let sub = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "" };
    if parsed.has_flag("help") || parsed.has_flag("h") || sub == "help" {
        usage();
        return 0;
    }
    if sub == "stats" { return cmd_stats(parsed); }
    if sub == "list" { return cmd_list(parsed); }
    if sub == "search" { return cmd_search(parsed); }
    if sub == "purge" { return cmd_purge(parsed); }
    if sub == "add" { return cmd_add(parsed); }
    if sub == "state" { return cmd_state(parsed); }
    eprintln!("unknown store subcommand: {}", sub);
    usage();
    return 1;
}

#[cfg(test)]
mod tests {
    use super::fmt_ms;
    use super::ingest;
    use super::Draft;
    use crate::cli::args::parse;
    use std::path::PathBuf;

    fn words(v: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v { out.push(s.to_string()); }
        return out;
    }

    fn draft(url: &str) -> Draft {
        return Draft {
            url: url.to_string(),
            title: "CLI 测试文档".to_string(),
            text: "cli body content 编译器".to_string(),
            query: "q".to_string(),
            source: "manual".to_string(),
            min_len: 0,
        };
    }

    #[test]
    fn html_sniffing_handles_missing_content_type() {
        assert!(super::looks_like_html("<!DOCTYPE html><html><body><p>hi</p></body></html>"));
        assert!(super::looks_like_html("  <html lang=\"zh\">   <body>x</body>"));
        assert!(!super::looks_like_html("# plain markdown\n\nsome text with <div> inline? no meta here"));
        assert!(!super::looks_like_html("{\"json\": true}"));
    }

    #[test]
    fn fmt_ms_is_utc() {
        assert_eq!(fmt_ms(0), "1970-01-01 00:00:00Z");
        assert_eq!(fmt_ms(86_400_000 + 3_600_000 + 60_000 + 1000), "1970-01-02 01:01:01Z");
        assert_eq!(fmt_ms(1_700_000_000_000).len(), 20);
    }

    #[test]
    fn ingest_second_run_is_all_skipped() {
        let kb = "_scratch/f_cli_ingest.jsonl";
        let p = PathBuf::from(kb);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_cli_ingest.state.toml");
        let parsed = parse(words(&["store", "add", "--kb", kb]));
        let first = ingest(&parsed, vec![draft("https://cli.example.com/x")]).unwrap();
        assert!(first.contains("1 stored / 0 skipped"), "{}", first);
        let second = ingest(&parsed, vec![draft("https://cli.example.com/x#frag")]).unwrap();
        assert!(second.contains("0 stored / 1 skipped"), "{}", second);
        assert!(second.contains("duplicate url"), "{}", second);
    }

    #[test]
    fn not_modified_skips_ingest() {
        let kb = "_scratch/f_cli_304.jsonl";
        let p = PathBuf::from(kb);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_cli_304.state.toml");
        let parsed = parse(words(&["store", "add", "--kb", kb]));
        let feed = "<rss><channel><item><title>t</title><description>d</description></item></channel></rss>";
        let rep = crate::fetch::http::Reply {
            url: "https://feed.example.com/rss".to_string(),
            final_url: "https://feed.example.com/rss".to_string(),
            status: 304,
            ok: true,
            not_modified: true,
            content_type: "text/plain".to_string(),
            etag: "W/\"x\"".to_string(),
            last_modified: String::new(),
            body: feed.to_string(),
            bytes: feed.len(),
            truncated: false,
            attempts: 1,
            error: String::new(),
        };
        let msg = super::ingest_reply(&parsed, &rep, feed).unwrap();
        assert!(msg.contains("unchanged"), "{}", msg);
        assert!(msg.contains("跳过入库"), "{}", msg);
        let s = crate::store::kb::Store::open(&p, 30).unwrap();
        assert_eq!(s.stats().docs, 0, "304 不应新增任何文档");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file("_scratch/f_cli_304.state.toml");
    }

    #[test]
    fn ttl_days_override_is_read() {
        let parsed = parse(words(&["store", "stats", "--kb", "_scratch/f_cli_ttl.jsonl", "--ttl-days", "7"]));
        assert_eq!(super::ttl_days(&parsed), 7);
        let dflt = parse(words(&["store", "stats"]));
        assert_eq!(super::ttl_days(&dflt), 30);
    }
}
