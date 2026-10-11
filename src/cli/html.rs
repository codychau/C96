//! C3 CLI: html 命令组 — 正文抽取。
//!   html extract --file PATH [--url URL] [--max-chars N] [--meta]

use crate::cli::args::Parsed;
use crate::cli::store::ingest;
use crate::cli::store::Draft;
use crate::fetch::html::article_from_html;
use crate::fetch::html::extract;
use crate::fetch::html::lang_of;

fn usage() {
    println!("usage: C96 html extract --file PATH [options]");
    println!("  --file PATH       required, local .html file");
    println!("  --url URL         optional page url (用于 site/lang 显示)");
    println!("  --max-chars N     print at most N chars of body text (0 = all)");
    println!("  --meta            print extraction metrics");
    println!("  --store           write the extracted body into the RAG knowledge base (F, needs --url)");
    println!("  --kb PATH         knowledge base JSONL (default %APPDATA%/com.c96.gui/kb.jsonl)");
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

pub fn run(parsed: &Parsed) -> i32 {
    if parsed.has_flag("help") || parsed.has_flag("h") {
        usage();
        return 0;
    }
    let path = parsed.opt_or("file", "");
    if path == "" {
        eprintln!("html extract: --file PATH is required");
        usage();
        return 1;
    }
    let text = match std::fs::read_to_string(&path) {
        Err(e) => { eprintln!("html extract: cannot read {}: {}", path, e); return 1; }
        Ok(t) => t,
    };
    let bom: &[u8] = &[0xEF, 0xBB, 0xBF];
    let bytes = text.as_bytes();
    let starts_bom = bytes.len() >= 3 && bytes[0] == bom[0] && bytes[1] == bom[1] && bytes[2] == bom[2];
    let body: &str = if starts_bom { &text[3..] } else { &text };
    let max_chars: usize = match parsed.opt("max-chars") {
        Some(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => { eprintln!("--max-chars requires an integer"); return 1; }
        },
        None => 0,
    };
    let url = parsed.opt_or("url", "");
    let article = if url != "" { Some(article_from_html(body, &url)) } else { None };
    let ex = extract(body);
    let title = if ex.title.is_empty() { "(no title)".to_string() } else { ex.title.clone() };
    println!("html: {}", title);
    if parsed.has_flag("meta") {
        let mut sel = format!("<{}", ex.tag);
        if !ex.id.is_empty() { sel = sel + " id=\"" + &ex.id + "\""; }
        if !ex.class.is_empty() { sel = sel + " class=\"" + &ex.class + "\""; }
        sel = sel + ">";
        let lang = match &article { Some(a) => a.lang.clone(), None => lang_of(body) };
        println!("  container:     {}", sel);
        println!("  text chars:    {}", ex.text_len);
        println!("  link density:  {:.3}", ex.link_density);
        if !lang.is_empty() { println!("  lang:          {}", lang); }
    }
    if let Some(a) = &article {
        println!("  url:           {}", a.url);
        println!("  site:          {}", a.site);
        if !a.title.is_empty() { println!("  article title: {}", a.title); }
    }
    if parsed.has_flag("store") {
        if url.is_empty() {
            eprintln!("html extract --store: --url URL is required (stable dedup key)");
            return 1;
        }
        let drafts = vec![Draft {
            url: url.clone(),
            title: ex.title.clone(),
            text: ex.text.clone(),
            query: "".to_string(),
            source: "html-file".to_string(),
            min_len: 0,
        }];
        match ingest(parsed, drafts) {
            Ok(s) => println!("{}", s),
            Err(e) => { eprintln!("store: {}", e); return 1; }
        }
    }
    let out = if max_chars > 0 { head_chars(&ex.text, max_chars) } else { ex.text.clone() };
    println!("  --- body ---");
    for line in out.split('\n') {
        println!("  {}", line);
    }
    println!("  --- {} char(s){} ---", ex.text_len, if max_chars > 0 && ex.text_len > max_chars { " (truncated)" } else { "" });
    return 0;
}
