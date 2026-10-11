//! `fetch` subcommand（C1 验收）: one URL, cache-aware conditional GET.

use crate::cli::args::Parsed;
use crate::cli::store::ingest_reply;
use crate::fetch::http::default_opts;
use crate::fetch::http::fetch;
use crate::fetch::cache::load;
use crate::fetch::cache::find;
use crate::fetch::cache::fold;
use crate::fetch::cache::save;

const CACHE_PATH: &str = "cache/http_cache.toml";
/// cache cap: newest entries survive（append order = newest last）
const MAX_ENTRIES: usize = 100;

fn num_opt(parsed: &Parsed, name: &str, dflt: usize) -> usize {
    let raw = parsed.opt_or(name, "");
    if raw == "" { return dflt; }
    match raw.parse::<usize>() {
        Ok(v) => { v }
        Err(_) => { println!("  ignoring --{}: not a number: {}", name, raw); dflt }
    }
}

fn dash(s: &str) -> String {
    if s.len() == 0 { return "-".to_string(); }
    return s.to_string();
}

fn head_chars(s: &str, n: usize) -> String {
    let mut out = "".to_string();
    let mut taken: usize = 0;
    for c in s.chars() {
        if taken >= n { break; }
        out.push(c);
        taken += 1;
    }
    return out;
}

pub fn usage() {
    println!("usage: C96 fetch --url URL [options]");
    println!("  --url URL         required, http(s)://");
    println!("  --timeout SECS    overall timeout, default 20");
    println!("  --connect SECS    connect timeout, default 6");
    println!("  --retries N       extra attempts on transport error, default 2");
    println!("  --max-body N      body cap in bytes, default 1048576");
    println!("  --cache PATH      cache file, default {}", CACHE_PATH);
    println!("  --no-cache        do not read or write the cache");
    println!("  --show-body       print the first 400 chars of the body");
    println!("  --store           write the parsed body into the RAG knowledge base (F)");
    println!("  --kb PATH         knowledge base JSONL (default %APPDATA%/com.c96.gui/kb.jsonl)");
    println!("  --ttl-days N      drop RAG docs older than N days, default 30");
}

pub fn run(parsed: &Parsed) -> i32 {
    let url = parsed.opt_or("url", "");
    if url == "" { usage(); return 1; }
    let mut opts = default_opts();
    opts.timeout_s = num_opt(parsed, "timeout", opts.timeout_s);
    opts.connect_s = num_opt(parsed, "connect", opts.connect_s);
    opts.retries = num_opt(parsed, "retries", opts.retries);
    opts.max_body = num_opt(parsed, "max-body", opts.max_body);
    let no_cache = parsed.has_flag("no-cache");
    let cache_path = parsed.opt_or("cache", &crate::cli::paths::resolve_default(CACHE_PATH));
    let entries = if no_cache { Vec::new() } else { load(&cache_path) };
    let cached = if no_cache { None } else { find(&entries, &url) };
    let c_etag = if cached.is_some() { cached.as_ref().unwrap().etag.to_string() } else { "".to_string() };
    let c_lm = if cached.is_some() { cached.as_ref().unwrap().last_modified.to_string() } else { "".to_string() };
    let c_body = if cached.is_some() { cached.as_ref().unwrap().body.to_string() } else { "".to_string() };
    if cached.is_some() {
        println!("cache entry for {}: etag={} last-modified={}",
            url, dash(&c_etag), dash(&c_lm));
        if c_etag.len() > 0 { opts.if_none_match = c_etag.to_string(); }
        if c_lm.len() > 0 { opts.if_modified_since = c_lm.to_string(); }
        if opts.if_none_match.len() > 0 { println!("  sending If-None-Match → conditional GET"); }
        if opts.if_modified_since.len() > 0 { println!("  sending If-Modified-Since → conditional GET"); }
    }
    let rep = fetch(&url, &opts);
    println!("GET {} → status {} ({} attempt(s))", rep.url, rep.status, rep.attempts);
    if rep.final_url.len() > 0 && rep.final_url != rep.url { println!("  redirected → {}", rep.final_url); }
    println!("  content-type={} etag={} last-modified={}",
        dash(&rep.content_type), dash(&rep.etag), dash(&rep.last_modified));
    if !rep.ok {
        eprintln!("  transport error after {} attempt(s): {}", rep.attempts, rep.error);
        eprintln!("hint: check the url, raise --timeout, or use --fixture replay (C4) offline");
        return 1;
    }
    if rep.not_modified {
        println!("  304 not modified → cached body reused ({} bytes), 0 bytes re-fetched", c_body.len());
    } else {
        println!("  body: {} bytes", rep.bytes);
        if rep.truncated { println!("  truncated at --max-body cap {}", opts.max_body); }
        if parsed.has_flag("show-body") { println!("  body head: {}", head_chars(&rep.body, 400)); }
    }
    if !no_cache {
        let next = fold(&entries, &url, &rep, MAX_ENTRIES);
        if next.len() > 0 {
            if !save(&cache_path, &next) { eprintln!("cannot write cache: {}", cache_path); return 1; }
            println!("cache: {} entry(s) → {}", next.len(), cache_path);
        }
    }
    if parsed.has_flag("store") {
        let body = if rep.not_modified { c_body.clone() } else { rep.body.clone() };
        if body.is_empty() {
            println!("store: skipped (empty body)");
        } else {
            match ingest_reply(parsed, &rep, &body) {
                Ok(s) => println!("{}", s),
                Err(e) => { eprintln!("store: {}", e); return 1; }
            }
        }
    }
    return 0;
}

#[cfg(test)]
mod tests {
    use crate::cli::args::parse;
    use crate::cli::http::head_chars;
    use crate::cli::http::num_opt;

    fn words(v: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v { out.push(s.to_string()); }
        return out;
    }

    #[test]
    fn head_chars_is_char_safe() {
        assert_eq!(head_chars("中文abc", 2), "中文");
        assert_eq!(head_chars("abc", 10), "abc");
        assert_eq!(head_chars("", 4), "");
    }

    #[test]
    fn num_opt_parses_and_defaults() {
        let p = parse(words(&["fetch", "--timeout", "9", "--retries", "abc"]));
        assert_eq!(num_opt(&p, "timeout", 20), 9);
        assert_eq!(num_opt(&p, "retries", 2), 2);
        assert_eq!(num_opt(&p, "connect", 6), 6);
    }
}
