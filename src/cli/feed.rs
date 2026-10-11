//! C2 CLI: <feed> command group.
//!   feed parse --file PATH [--limit N]   parse a local RSS 2.0 / Atom file -> articles
//!
//! uses crate::fetch::feed (RSS 2.0 + Atom -> unified Article).

use crate::cli::args::Parsed;
use crate::cli::store::ingest;
use crate::cli::store::Draft;
use crate::fetch::feed::parse_feed;

/// `feed parse --file PATH [--limit N]` -> structured articles, exit 0.
pub fn run(parsed: &Parsed) -> i32 {
    if parsed.has_flag("help") || parsed.has_flag("h") {
        println!("feed parse --file PATH [--limit N]");
        println!("  parse a local RSS 2.0 / Atom feed and print structured articles");
        println!("  --file PATH   required, path to the .xml/.rss/.atom file");
        println!("  --limit N     print at most N articles (0 = all)");
        println!("  --store       write every parsed article into the RAG knowledge base (F)");
        println!("  --kb PATH     knowledge base JSONL (default %APPDATA%/com.c96.gui/kb.jsonl)");
        return 0;
    }
    let path: String = match parsed.opt("file") {
        Some(p) => p,
        None => {
            eprintln!("feed parse: --file PATH is required");
            return 1;
        }
    };
    let limit: usize = match parsed.opt("limit") {
        Some(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => { eprintln!("--limit requires an integer"); return 1; }
        },
        None => 0,
    };
    let text = match std::fs::read_to_string(&path) {
        Err(e) => {
            eprintln!("feed parse: cannot read {}: {}", path, e);
            return 1;
        }
        Ok(t) => t,
    };
    let bom: &[u8] = &[0xEF, 0xBB, 0xBF];
    let bytes = text.as_bytes();
    let starts_bom = bytes.len() >= 3 && bytes[0] == bom[0] && bytes[1] == bom[1] && bytes[2] == bom[2];
    let body: &str = if starts_bom { &text[3..] } else { &text };
    let feed = match parse_feed(body) {
        Err(e) => {
            eprintln!("feed parse: {}", e);
            return 1;
        }
        Ok(f) => f,
    };
    println!("feed: {} ({} item(s))", dis(&feed.title), feed.items.len());
    if feed.language.len() > 0 { println!("  language: {}", feed.language); }
    if feed.updated.len() > 0 { println!("  updated:  {}", dis(&feed.updated)); }
    let mut shown: usize = 0;
    for art in &feed.items {
        if limit > 0 && shown >= limit { break; }
        shown += 1;
        println!();
        println!("  {}. {}", shown, dis(&art.title));
        println!("     url:     {}", art.url);
        if art.site.len() > 0 { println!("     site:    {}", art.site); }
        if art.lang.len() > 0 { println!("     lang:    {}", art.lang); }
        if art.published.len() > 0 { println!("     published: {}", dis(&art.published)); }
        let b = art.body.replace('\t', " ");
        let blen = b.chars().count();
        let body_disp = if blen > 240 { &b[..240] } else { &b };
        let ell = if blen > 240 { "..." } else { "" };
        println!("     body:    {}{}", body_disp, ell);
    }
    println!();
    println!("  ({} shown, {} total)", shown, feed.items.len());
    if parsed.has_flag("store") {
        let mut drafts: Vec<Draft> = Vec::new();
        for a in &feed.items {
            drafts.push(Draft {
                url: a.url.clone(),
                title: a.title.clone(),
                text: a.body.clone(),
                query: "".to_string(),
                source: "feed-file".to_string(),
                min_len: 0,
            });
        }
        match ingest(parsed, drafts) {
            Ok(s) => println!("{}", s),
            Err(e) => { eprintln!("store: {}", e); return 1; }
        }
    }
    return 0;
}

/// strip a leading BOM marker left on a string slice (used for non-first fields).
fn dis(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF {
        return s[3..].to_string();
    }
    return s.to_string();
}

