//! 阶段 G CLI（精简版）：\`index stats\` / \`index query\` / \`index bench\`。
//!
//! 索引不落盘：每次进程启动惰性重建（Store::search_index），查询走 BM25。

use std::path::PathBuf;
use std::time::Instant;

use crate::cli::args::Parsed;
use crate::cli::store::kb_path;
use crate::cli::store::ttl_days;
use crate::index::Index;
use crate::research::retrieve;
use crate::store::kb::Store;

fn usage() {
    println!("usage: C96 index <subcommand> [options]");
    println!("  index stats [--kb PATH] [--json]              索引规模 / 构建耗时");
    println!("  index query --query Q [--limit N] [--kb PATH] [--json]   BM25 检索（含 snippet）");
    println!("  index bench [--docs N] [--queries N]          合成语料 benchmark（验收：10k docs <20ms/query）");
    println!("  （词项 = 字符码点整数；权重 标题x3 / 正文x1；BM25 k1=1.2 b=0.75 + 短语加成）");
}

fn num_opt(parsed: &Parsed, name: &str, dflt: u64) -> u64 {
    let raw = parsed.opt_or(name, "");
    if raw.is_empty() { return dflt; }
    match raw.parse::<u64>() {
        Ok(v) => { return v; }
        Err(_) => { println!("  ignoring --{}: not a number: {}", name, raw); return dflt; }
    }
}

fn open(parsed: &Parsed) -> Result<Store, String> {
    return Store::open(&PathBuf::from(kb_path(parsed)), ttl_days(parsed));
}

pub fn run(parsed: &Parsed) -> i32 {
    let sub = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "stats" };
    if parsed.has_flag("help") || parsed.has_flag("h") || sub == "help" {
        usage();
        return 0;
    }
    if sub == "stats" { return stats(parsed); }
    if sub == "query" || sub == "search" { return query(parsed); }
    if sub == "bench" { return bench(parsed); }
    eprintln!("unknown index subcommand: {}", sub);
    usage();
    return 1;
}

fn stats(parsed: &Parsed) -> i32 {
    let mut s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("index stats: {}", e); return 1; }
    };
    let json = parsed.has_flag("json");
    let st = s.search_index().stats();
    if json {
        println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default());
        return 0;
    }
    println!("index: {} 文档 / {} 词项 / {} postings / avg_len {:.1} / 构建 {} ms",
        st.docs, st.terms, st.postings, st.avg_len, st.build_ms);
    println!("  kb: {}", kb_path(parsed));
    return 0;
}

fn query(parsed: &Parsed) -> i32 {
    let q = parsed.opt_or("query", "");
    if q.trim().is_empty() { eprintln!("index query: --query TEXT is required"); return 1; }
    let limit = num_opt(parsed, "limit", 10) as usize;
    let json = parsed.has_flag("json");
    let mut s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("index query: {}", e); return 1; }
    };
    let st = s.search_index().stats(); // 先建索引（build_ms 单独统计）
    let t0 = Instant::now();
    let hits = retrieve(&mut s, &q, limit);
    let qus = t0.elapsed().as_micros();
    if json {
        println!("{}", serde_json::to_string_pretty(&hits).unwrap_or_default());
        return 0;
    }
    println!("index query \"{}\": {} 命中 | {} 文档 / {} 词项 / build {} ms | 检索 {} µs",
        q, hits.len(), st.docs, st.terms, st.build_ms, qus);
    for (i, h) in hits.iter().enumerate() {
        println!("  {}. score={} [{}] {}", i + 1, h.score, h.source, h.host);
        if !h.title.is_empty() { println!("     {}", h.title); }
        println!("     {}", h.url);
        if !h.snippet.is_empty() { println!("     …{}…", h.snippet); }
    }
    return 0;
}

fn rnd(seed: &mut u64) -> usize {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    return (*seed >> 33) as usize;
}

fn bench(parsed: &Parsed) -> i32 {
    let docs_n = num_opt(parsed, "docs", 10_000) as usize;
    let queries_n = num_opt(parsed, "queries", 200) as usize;
    if docs_n == 0 { eprintln!("index bench: --docs must be > 0"); return 1; }
    let cjk: Vec<char> = "卡普空怪物猎人游戏最新发布预告生化危机街头霸王任天堂塞尔达马力欧最终幻想".chars().collect();
    let words = ["capcom", "monster", "hunter", "resident", "evil", "street", "fighter", "game", "news", "trailer", "switch", "review"];
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut items: Vec<(String, String, String)> = Vec::with_capacity(docs_n);
    for i in 0..docs_n {
        let mut title = String::new();
        let mut text = String::new();
        for _ in 0..6 {
            let k = rnd(&mut seed);
            title.push(cjk[k % cjk.len()]);
        }
        for _ in 0..220 {
            let k = rnd(&mut seed);
            if k % 3 == 0 {
                text.push_str(words[rnd(&mut seed) % words.len()]);
                text.push(' ');
            } else {
                text.push(cjk[k % cjk.len()]);
            }
        }
        items.push((format!("https://example.com/{}", i), title, text));
    }
    let text_bytes: usize = items.iter().map(|t| t.1.len() + t.2.len()).sum();
    let refs: Vec<(&str, &str, &str)> = items.iter().map(|t| (t.0.as_str(), t.1.as_str(), t.2.as_str())).collect();
    let t0 = Instant::now();
    let idx = Index::build_texts(refs.into_iter());
    let build = t0.elapsed();
    let queries: Vec<String> = (0..queries_n.max(1)).map(|i| items[i % items.len()].1.clone()).collect();
    let t1 = Instant::now();
    let mut hit_total = 0usize;
    for q in &queries {
        hit_total += idx.search(q, 10).len();
    }
    let qms = t1.elapsed();
    let avg_ms = qms.as_secs_f64() * 1000.0 / queries_n.max(1) as f64;
    let st = idx.stats();
    println!("index bench: {} 文档（文本 ≈ {} KiB）", st.docs, text_bytes / 1024);
    println!("  词项 {} / postings {} / avg_len {:.1}", st.terms, st.postings, st.avg_len);
    println!("  构建 {} ms（{:.1} µs/doc）| 查询 {} 次 {} ms -> 平均 {:.3} ms/次 | 累计命中 {}",
        build.as_millis(), build.as_micros() as f64 / st.docs.max(1) as f64,
        queries_n, qms.as_millis(), avg_ms, hit_total);
    if avg_ms < 20.0 {
        println!("  验收 G：10k 文档平均查询 {:.3} ms < 20 ms ✅", avg_ms);
    } else {
        println!("  验收 G：平均查询 {:.3} ms >= 20 ms ❌", avg_ms);
    }
    return 0;
}
