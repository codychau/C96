//! 'feeds list' / 'feeds add'（阶段 B 验收命令）.

use crate::config::toml::parse;
use crate::config::pref::from_doc;
use crate::config::pref::builtin_defaults;
use crate::config::pref::check_feed;
use crate::config::pref::count_errors;
use crate::config::pref::count_warnings;
use crate::config::pref::kind_text;
use crate::config::pref::fmt_num;
use crate::config::pref::render_feed;
use crate::config::pref::split_comma_list;
use crate::config::pref::Feed;
use crate::config::pref::Prefs;
use crate::cli::args::Parsed;

/// list：读取 config、解析、校验、打印偏好. 返回 exit code（0 ok / 1 fatal）.
pub fn list(path: &str) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => { s }
        Err(_) => {
            eprintln!("cannot read config: {}", path);
            eprintln!("hint: feeds add --url https://... --engine bing --keywords rust,cargo");
            return 1;
        }
    };
    let doc = match parse(&text) {
        Ok(d) => { d }
        Err(e) => {
            eprintln!("config parse error (line {}): {}", e.line, e.message);
            eprintln!("hint: one key = value per line; strings need double quotes; each feed is a [[feeds]] block");
            return 1;
        }
    };
    let prefs = from_doc(&doc);
    let d = prefs.defaults;
    println!("C96 — 偏好 (config: {})", path);
    println!("defaults: engine={} weight={} max_age_hours={} site={} blacklist={} keywords={}",
        d.engine, fmt_num(d.weight), d.max_age_hours, dash_or(&d.site), d.blacklist.len(),
        dash_or_list(&d.keywords));
    if prefs.feeds.is_empty() {
        println!("(no feeds yet — use feeds add)");
    }
    let mut idx: usize = 0;
    for f in &prefs.feeds {
        idx += 1;
        println!("{:>3} {:<24} {:<6} {:>5} {:>6}h {:<16} {}", idx, f.name, f.engine,
            fmt_num(f.weight), f.max_age_hours, dash_or(&f.site), dash_or_list(&f.keywords));
        println!("    url: {}", f.url);
        if !f.blacklist.is_empty() {
            println!("    blacklist: {}", dash_or_list(&f.blacklist));
        }
        if !f.enabled {
            println!("    (disabled — refresh 跳过)");
        }
        if !f.note.is_empty() {
            println!("    note: {}", f.note);
        }
    }
    let errors = count_errors(&prefs.issues);
    let warnings = count_warnings(&prefs.issues);
    if errors + warnings > 0 {
        println!("issues: {} error / {} warning", errors, warnings);
        for it in &prefs.issues {
            let at = if it.line > 0 { format!("line {}", it.line) } else { "config".to_string() };
            println!("  {} ({}): {}", kind_text(it.kind), at, it.message);
        }
    }
    println!("feeds: {} total / enabled {} / disabled {}", prefs.feeds.len(),
        count_enabled(&prefs.feeds), count_disabled(&prefs.feeds));
    if errors > 0 {
        eprintln!("fix the errors above (exit 1)");
        return 1;
    }
    return 0;
}

/// add：validate flags -> append one [[feeds]] block（atomic tmp+rename）.
pub fn add(path: &str, parsed: &Parsed) -> i32 {
    let url = parsed.opt_or("url", "");
    if url.is_empty() {
        eprintln!("feeds add needs --url <https://...>");
        eprintln!("example: feeds add --url https://www.bing.com/news/search?q=rust --engine bing --keywords rust,cargo --weight 2");
        return 1;
    }
    let d = builtin_defaults();
    let mut f = Feed {
        name: parsed.opt_or("name", ""),
        url: url.clone(),
        engine: parsed.opt_or("engine", &d.engine),
        site: parsed.opt_or("site", &d.site),
        keywords: split_comma_list(&parsed.opt_or("keywords", "")),
        blacklist: split_comma_list(&parsed.opt_or("blacklist", "")),
        weight: d.weight,
        max_age_hours: d.max_age_hours,
        enabled: !parsed.has_flag("disabled"),
        note: parsed.opt_or("note", ""),
        line: 0,
    };
    let w_text = parsed.opt_or("weight", "");
    if !w_text.is_empty() {
        match w_text.parse::<f64>() {
            Ok(v) => { f.weight = v; }
            Err(_) => {
                eprintln!("--weight must be a number in (0, 10]: got {}", w_text);
                return 1;
            }
        }
    }
    let age_text = if parsed.opt("max-age-hours").is_some() { parsed.opt_or("max-age-hours", "") } else { parsed.opt_or("max-age", "") };
    if !age_text.is_empty() {
        match age_text.parse::<i64>() {
            Ok(v) => { f.max_age_hours = v; }
            Err(_) => {
                eprintln!("--max-age-hours must be an integer >= 1: got {}", age_text);
                return 1;
            }
        }
    }
    let issues = check_feed(&f);
    if count_errors(&issues) > 0 {
        println!("rejected (not written):");
        for i in &issues {
            println!("  {} : {}", kind_text(i.kind), i.message);
        }
        return 1;
    }
    let block = render_feed(&f);
    if parsed.has_flag("dry-run") {
        println!("(dry-run) would append to {}", path);
        print_block(&block);
        return 0;
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => { s }
        Err(_) => { "# C96 user preferences\n# [default] = global defaults, [[feeds]] = one block per subscription\n\n".to_string() }
    };
    if !parsed.has_flag("force") {
        let prior = match parse(&existing) {
            Ok(pd) => { from_doc(&pd) }
            Err(_) => { Prefs { defaults: builtin_defaults(), feeds: Vec::new(), issues: Vec::new() } }
        };
        for g in &prior.feeds {
            if g.url == url {
                eprintln!("that url is already in the config (line {}): {}", g.line, g.url);
                eprintln!("use --force to add it anyway, or feeds list to review");
                return 1;
            }
        }
    }
    let mut merged = existing;
    if !merged.is_empty() && !merged.ends_with("\n") {
        merged += "\n";
    }
    if !merged.is_empty() {
        merged += "\n";
    }
    merged = merged + &block;
    let tmp = path.to_owned() + ".tmp";
    match std::fs::write(&tmp, merged.as_bytes()) {
        Ok(_) => {}
        Err(_) => {
            eprintln!("cannot write {}", tmp);
            return 1;
        }
    }
    match std::fs::rename(tmp, path) {
        Ok(_) => {}
        Err(_) => {
            eprintln!("cannot replace {}", path);
            return 1;
        }
    }
    println!("added 1 feed -> {}", path);
    print_block(&block);
    return 0;
}

fn count_enabled(feeds: &[Feed]) -> usize {
    let mut n: usize = 0;
    for f in feeds {
        if f.enabled {
            n += 1;
        }
    }
    return n;
}

fn count_disabled(feeds: &[Feed]) -> usize {
    let mut n: usize = 0;
    for f in feeds {
        if !f.enabled {
            n += 1;
        }
    }
    return n;
}

fn print_block(block: &str) {
    for line in block.lines() {
        println!("  {}", line);
    }
}

fn dash_or(s: &str) -> String {
    if s.is_empty() {
        return "-".to_string();
    }
    return s.to_string();
}

fn dash_or_list(v: &[String]) -> String {
    if v.is_empty() {
        return "-".to_string();
    }
    return v.join(" | ");
}
