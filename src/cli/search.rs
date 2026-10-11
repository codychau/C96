//! D CLI: search — 从偏好或 --query 生成 plan，跑 Bing/Baidu，归一化 + 白黑名单 + 去重。
//!
//! 在线：Bing News RSS（快） + Bing SERP + Baidu SERP（BAIDUID cookie）。
//! 离线：--from FILE（保存的 RSS/SERP HTML），用于可复现验证与测试。

use crate::cli::args::Parsed;
use crate::cli::store::ingest;
use crate::cli::store::Draft;
use crate::config::pref::from_doc;
use crate::config::pref::split_comma_list;
use crate::config::toml::parse as parse_toml;
use crate::search::baidu;
use crate::search::bing;
use crate::search::normalize;
use crate::search::query;
use crate::search::run as engine_run;
use crate::search::Candidate;

const DEFAULT_CONFIG: &str = "config/feeds.toml";

fn usage() {
    println!("usage: C96 search [--query Q | --feed NAME] [options]");
    println!("  --query TEXT      one-off query (keyword)");
    println!("  --feed NAME       build keyword×site×engine from [[feeds]] in --config");
    println!("  --config PATH     config file, default {}", DEFAULT_CONFIG);
    println!("  --engine bing|baidu|auto   default auto (bing+baidu)");
    println!("  --site HOST       restrict to a site (adds site:HOST; also client whitelist)");
    println!("  --max N           at most N candidates to print, default 20");
    println!("  --from FILE       parse a saved Bing RSS/SERP or Baidu SERP file (offline)");
    println!("  --timeout SECS    overall timeout, default 20");
    println!("  --retries N       extra attempts on transport error, default 1");
    println!("  --cookie STR      override the Cookie header (default: Baidu BAIDUID)");
    println!("  --store           write unique candidates (title+snippet) into the RAG knowledge base (F)");
    println!("  --kb PATH         knowledge base JSONL (default %APPDATA%/com.c96.gui/kb.jsonl)");
}

fn num_opt(parsed: &Parsed, name: &str, dflt: usize) -> usize {
    let raw = parsed.opt_or(name, "");
    if raw == "" { return dflt; }
    match raw.parse::<usize>() {
        Ok(v) => { v }
        Err(_) => { println!("  ignoring --{}: not a number: {}", name, raw); dflt }
    }
}

fn dash(s: &str) -> String {
    if s.is_empty() { return "-".to_string(); }
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

fn looks_xml(text: &str) -> bool {
    let t = text.trim_start();
    return t.starts_with("<?xml") || t.starts_with("<rss") || t.contains("<rss ");
}

fn detect_kind(text: &str, engine: &str) -> String {
    let e = engine.trim().to_lowercase();
    if e == "bing" {
        if looks_xml(text) { return "rss".to_string(); }
        return "bing".to_string();
    }
    if e == "baidu" { return "baidu".to_string(); }
    if looks_xml(text) { return "rss".to_string(); }
    if text.contains("b_algo") { return "bing".to_string(); }
    if text.contains("c-container") || text.contains("baidu.com") { return "baidu".to_string(); }
    return "".to_string();
}

/// 离线 --from 路径的一行报告（在线路径的报告由 search::run::collect 返回）。
fn report(engine: &str, kind: &str, status: usize, n: usize) {
    println!("  {:<5} {:<4} http {} → {} candidate(s)", engine, kind, status, n);
}

pub fn run(parsed: &Parsed) -> i32 {
    if parsed.has_flag("help") || parsed.has_flag("h") {
        usage();
        return 0;
    }
    let from = parsed.opt_or("from", "");
    let mut engine = parsed.opt_or("engine", "auto");
    let mut site = parsed.opt_or("site", "");
    let mut blacklist: Vec<String> = Vec::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut plan_from_cfg: Option<query::Plan> = None;
    let feed_name = parsed.opt_or("feed", "");
    if feed_name != "" {
        let cfg = parsed.opt_or("config", &crate::cli::paths::resolve_default(DEFAULT_CONFIG));
        let text = match std::fs::read_to_string(&cfg) {
            Ok(t) => t,
            Err(e) => { eprintln!("search: cannot read config {}: {}", cfg, e); return 1; }
        };
        let doc = match parse_toml(&text) {
            Ok(d) => d,
            Err(e) => { eprintln!("search: config parse error (line {}): {}", e.line, e.message); return 1; }
        };
        let prefs = from_doc(&doc);
        let mut found: Option<usize> = None;
        for (i, f) in prefs.feeds.iter().enumerate() {
            if f.name == feed_name || f.url == feed_name { found = Some(i); break; }
        }
        let idx = match found {
            Some(i) => i,
            None => {
                eprintln!("search: no [[feeds]] named \"{}\" in {}", feed_name, cfg);
                let mut names: Vec<String> = Vec::new();
                for f in &prefs.feeds { names.push(f.name.clone()); }
                if !names.is_empty() { eprintln!("  available: {}", names.join(", ")); }
                return 1;
            }
        };
        let f = &prefs.feeds[idx];
        blacklist = f.blacklist.clone();
        if site == "" && engine.trim().to_lowercase() == "auto" {
            // D1：无 CLI 覆盖时直接从偏好生成 plan（keyword × site × engine）
            plan_from_cfg = Some(query::plan_from_feed(f));
        } else {
            keywords = f.keywords.clone();
            if site == "" { site = f.site.clone(); }
            if engine.trim().to_lowercase() == "auto" { engine = f.engine.clone(); }
        }
    } else {
        let q = parsed.opt_or("query", "");
        if q == "" && from == "" {
            eprintln!("search: --query TEXT, --feed NAME or --from FILE is required");
            usage();
            return 1;
        }
        if q != "" { keywords.push(q); }
    }
    let mut plan = match plan_from_cfg {
        Some(p) => p,
        None => query::build_plan(&keywords, &site, &engine),
    };
    if plan.query.is_empty() {
        if from.is_empty() {
            eprintln!("search: empty query (feed has no keywords and no --query)");
            return 1;
        }
        plan.query = "(offline)".to_string();
    }
    let max: usize = num_opt(parsed, "max", 20);
    let timeout: usize = num_opt(parsed, "timeout", 20);
    let retries: usize = num_opt(parsed, "retries", 1);
    let cookie = parsed.opt_or("cookie", "");
    let sites: Vec<String> = split_comma_list(&plan.site);

    println!("search: q=\"{}\" engine={} site={} max={} kw={}", plan.query, plan.engine, dash(&plan.site), max, plan.keywords.len());

    let mut all: Vec<Candidate> = Vec::new();
    if from != "" {
        let text = match std::fs::read_to_string(&from) {
            Ok(t) => t,
            Err(e) => { eprintln!("search: cannot read --from {}: {}", from, e); return 1; }
        };
        let kind = detect_kind(&text, &plan.engine);
        if kind == "rss" {
            let c = bing::rss_candidates(&text);
            report("bing", "rss", 200, c.len());
            all.extend(c);
        } else if kind == "bing" {
            let c = bing::parse_serp(&text);
            report("bing", "serp", 200, c.len());
            all.extend(c);
        } else if kind == "baidu" {
            let c = baidu::parse_serp(&text);
            report("baidu", "serp", 200, c.len());
            all.extend(c);
        } else {
            eprintln!("search: --from {}: cannot detect format; pass --engine bing|baidu", from);
            return 1;
        }
    } else {
        let engines = query::engines(&plan.engine);
        for e in &engines {
            if e != "bing" && e != "baidu" {
                eprintln!("search: unknown engine \"{}\" (bing|baidu|auto)", e);
                return 1;
            }
        }
        let ro = engine_run::RunOpts { timeout_s: timeout, retries: retries, cookie: cookie.clone() };
        let (cands, reports) = engine_run::collect(&plan, max, &ro);
        for r in &reports {
            if r.ok {
                println!("  {:<5} {:<4} http {} → {} candidate(s)", r.engine, r.kind, r.status, r.count);
            } else if r.status == 0 {
                eprintln!("  {:<5} {:<4} failed: {}", r.engine, r.kind, r.error);
            } else {
                println!("  {:<5} {:<4} http {} → 0 candidate(s)", r.engine, r.kind, r.status);
            }
        }
        all.extend(cands);
    }

    let raw_total = all.len();
    let mut kept: Vec<Candidate> = Vec::new();
    let mut dropped_black: usize = 0;
    let mut dropped_white: usize = 0;
    for c in all {
        if !blacklist.is_empty() && normalize::is_blacklisted(&c.url, &blacklist) {
            dropped_black += 1;
            continue;
        }
        if !sites.is_empty() && !normalize::is_whitelisted(&c.site, &sites) {
            dropped_white += 1;
            continue;
        }
        kept.push(c);
    }
    let uniq = normalize::dedup(kept);
    let mut n_bing: usize = 0;
    let mut n_baidu: usize = 0;
    for c in &uniq {
        if c.source.starts_with("baidu") { n_baidu += 1; } else { n_bing += 1; }
    }
    println!("candidates: {} raw → {} unique (bing {} / baidu {}){}",
        raw_total, uniq.len(), n_bing, n_baidu,
        if dropped_black + dropped_white > 0 {
            format!(" [filtered: {} blacklist, {} off-site]", dropped_black, dropped_white)
        } else {
            "".to_string()
        });
    let mut shown: usize = 0;
    for c in &uniq {
        if shown >= max { break; }
        shown += 1;
        println!();
        println!("  {}. [{} #{}] {}", shown, c.source, c.rank, c.title);
        let note = if c.resolved { "" } else { "   (unresolved redirect)" };
        println!("     url:  {}{}", c.url, note);
        if !c.site.is_empty() { println!("     site: {}", c.site); }
        if !c.snippet.is_empty() { println!("     snip: {}", head_chars(&c.snippet, 160)); }
    }
    println!();
    println!("  ({} shown, {} unique of {} raw)", shown, uniq.len(), raw_total);
    if parsed.has_flag("store") {
        let mut drafts: Vec<Draft> = Vec::new();
        for c in &uniq {
            drafts.push(Draft {
                url: c.url.clone(),
                title: c.title.clone(),
                text: c.snippet.clone(),
                query: plan.query.clone(),
                source: c.source.clone(),
                min_len: 0,
            });
        }
        match ingest(parsed, drafts) {
            Ok(s) => println!("{}", s),
            Err(e) => { eprintln!("store: {}", e); return 1; }
        }
    }
    if uniq.is_empty() { return 1; }
    return 0;
}

#[cfg(test)]
mod tests {
    use super::detect_kind;
    use super::head_chars;
    use super::num_opt;
    use crate::cli::args::parse;

    fn words(v: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v { out.push(s.to_string()); }
        return out;
    }

    #[test]
    fn kind_detection() {
        assert_eq!(detect_kind("<?xml version=\"1.0\"?><rss>", "auto"), "rss");
        assert_eq!(detect_kind("<html><li class=\"b_algo\">", "auto"), "bing");
        assert_eq!(detect_kind("<html><div class=\"c-container\">", "auto"), "baidu");
        assert_eq!(detect_kind("<html>plain", "baidu"), "baidu");
        assert_eq!(detect_kind("<html>plain", "bing"), "bing");
    }

    #[test]
    fn head_chars_is_char_safe() {
        assert_eq!(head_chars("中文abc", 2), "中文");
        assert_eq!(head_chars("abc", 10), "abc");
    }

    #[test]
    fn num_opt_fallback() {
        let p = parse(words(&["search", "--max", "5", "--timeout", "x"]));
        assert_eq!(num_opt(&p, "max", 20), 5);
        assert_eq!(num_opt(&p, "timeout", 20), 20);
    }
}
