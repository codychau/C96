//! 阶段 I CLI：\`research\`（主题订阅：首搜即入库 / 命中读 RAG / 过期再搜） + \`topics\`（任务清单）。
//!
//! 用户原则落地：
//!   - 没搜过 -> \`research --query Q\` 立刻搜索 + 建任务 + 入 RAG；
//!   - 搜过   -> 直接读 RAG（state=hit）；数据太旧（state=stale）先给旧数据再刷新一次；
//!   - 常驻   -> \`research --query Q --watch\` 或 GUI 里持续运行，按 interval 到期自动刷新。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use crate::cli::args::Parsed;
use crate::cli::store::fmt_ms;
use crate::cli::store::kb_path;
use crate::cli::store::ttl_days;
use crate::research;
use crate::research::Policy;
use crate::research::RunOpts;
use crate::research::ServeResult;
use crate::research::TopicRun;
use crate::research::TopicSpec;
use crate::store::doc::now_ms;
use crate::store::kb::Store;
use crate::store::state;

fn usage() {
    println!("usage: C96 research --query Q [options]");
    println!("  --query TEXT      关键词（必需）");
    println!("  --engine auto|bing|baidu   default auto");
    println!("  --site HOST       站点限制（可选）");
    println!("  --max N           候选上限，default 20");
    println!("  --fetch N         每次刷新抓取的正文页数，default 5");
    println!("  --interval SECS   刷新间隔，default {}（到点自动再搜）", research::DEFAULT_INTERVAL_SECS);
    println!("  --fresh SECS      新鲜期，default {}（期内直接读 RAG，不联网）", research::DEFAULT_FRESH_SECS);
    println!("  --json            输出 JSON（给 AI / 脚本）");
    println!("  --cache-only      完全不联网，只读 RAG");
    println!("  --watch [--tick SECS]  常驻：到期自动刷新（tick default {}）", research::DEFAULT_SCHED_TICK_SECS);
    println!("  --kb PATH / --ttl-days N   同 store");
    println!();
    println!("usage: C96 topics <subcommand> [QUERY]");
    println!("  topics list [--json]        任务清单（关键词/运行次数/入库数/下次到期）");
    println!("  topics show QUERY           单个任务详情");
    println!("  topics run QUERY            立即刷新一次");
    println!("  topics pause|resume QUERY   暂停 / 恢复（恢复后立即到期）");
    println!("  topics rm QUERY             删除任务（RAG 文档保留，等 TTL）");
    println!("  topics due                  当前到期的任务");
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
    return num_opt(parsed, name, dflt as u64) as usize;
}

fn open(parsed: &Parsed) -> Result<Store, String> {
    return Store::open(&PathBuf::from(kb_path(parsed)), ttl_days(parsed));
}

fn spec_of(parsed: &Parsed) -> Result<TopicSpec, String> {
    let q = parsed.opt_or("query", "");
    if q.trim().is_empty() { return Err("--query TEXT is required".to_string()); }
    let mut s = TopicSpec::new(&q);
    s.engine = parsed.opt_or("engine", "auto");
    s.site = parsed.opt_or("site", "");
    s.max = usize_opt(parsed, "max", s.max);
    s.fetch_n = usize_opt(parsed, "fetch", s.fetch_n);
    s.interval_secs = num_opt(parsed, "interval", s.interval_secs);
    s.fresh_secs = num_opt(parsed, "fresh", s.fresh_secs);
    return Ok(s);
}

fn run_opts(parsed: &Parsed) -> RunOpts {
    let mut o = RunOpts::new();
    o.net.timeout_s = usize_opt(parsed, "timeout", 20);
    o.net.retries = usize_opt(parsed, "retries", 1);
    o.net.cookie = parsed.opt_or("cookie", "");
    o.min_len_page = usize_opt(parsed, "min-len", 40);
    return o;
}

fn print_run(r: &TopicRun) {
    println!("  刷新: 候选 {} → 唯一 {} | 抓取 {} 页 | 入库 {} | 跳过 {} | 未变 {} | 失败 {} | {} ms | status={}",
        r.candidates, r.unique, r.fetched, r.stored, r.skipped, r.unchanged, r.failed, r.ms, r.status);
    for e in &r.reports {
        if e.ok {
            println!("    {} {} http {} → {} candidate(s)", e.engine, e.kind, e.status, e.count);
        } else if e.status == 0 {
            println!("    {} {} failed: {}", e.engine, e.kind, e.error);
        } else {
            println!("    {} {} http {} → 0 candidate(s)", e.engine, e.kind, e.status);
        }
    }
    if !r.error.is_empty() { println!("    error: {}", r.error); }
}

fn print_serve(r: &ServeResult) {
    let age_h = r.age_ms / 3_600_000;
    println!("research \"{}\": state={} from_cache={} age={}h", r.query, r.state, r.from_cache, age_h);
    println!("  topic: runs={} stored={} interval={}s fresh={}s next_due={} last_run={}",
        r.topic.runs, r.topic.stored, r.topic.interval_secs, r.topic.fresh_secs,
        fmt_ms(r.topic.next_due_ms), fmt_ms(r.topic.last_run_ms));
    if let Some(run) = &r.run { print_run(run); }
    if let Some(k) = &r.pending {
        println!("  pending refresh（后台再搜一次）: {}", k);
    }
    println!("  RAG hits: {}", r.hits.len());
    for (i, h) in r.hits.iter().take(10).enumerate() {
        println!("    {}. score={} [{}] {} ({})", i + 1, h.score, h.source, h.host, fmt_ms(h.stored_at_ms));
        if !h.title.is_empty() { println!("       {}", h.title); }
        println!("       {}", h.url);
    }
    if r.state == "new" && r.hits.is_empty() && r.run.is_none() {
        println!("  （首次任务已建立，尚未搜索；加 --watch 或直接再跑一次即会刷新）");
    }
}

fn print_topic(t: &state::Topic) {
    let now = now_ms();
    let due = if state::is_due(t, now) { "DUE" } else { "wait" };
    println!("  [{}] \"{}\" engine={} site={} runs={} stored={} interval={}s fresh={}s paused={}",
        due, t.query, t.engine, if t.site.is_empty() { "-" } else { &t.site },
        t.runs, t.stored, t.interval_secs, t.fresh_secs, t.paused);
    println!("      key={} max={} fetch_n={}", t.key, t.max, t.fetch_n);
    println!("      created={} last_run={} next_due={} status={} docs={}",
        fmt_ms(t.created_ms), fmt_ms(t.last_run_ms), fmt_ms(t.next_due_ms),
        if t.last_status.is_empty() { "-" } else { &t.last_status }, t.docs.len());
    if !t.last_error.is_empty() { println!("      last_error={}", t.last_error); }
}

fn query_arg(parsed: &Parsed) -> String {
    if parsed.positional.len() > 2 { return parsed.positional[2].clone(); }
    return parsed.opt_or("query", "");
}

pub fn run(parsed: &Parsed) -> i32 {
    if parsed.has_flag("help") || parsed.has_flag("h") {
        usage();
        return 0;
    }
    let spec = match spec_of(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("research: {}", e); usage(); return 1; }
    };
    let json = parsed.has_flag("json");
    let ro = run_opts(parsed);
    let mut s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("research: {}", e); return 1; }
    };
    let watch = parsed.has_flag("watch");
    let policy = if parsed.has_flag("cache-only") { Policy::cache_only() } else { Policy::cli() };
    let now = now_ms();
    match research::handle_query(&mut s, &spec, &ro, &policy, now) {
        Ok(r) => {
            if json { println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default()); }
            else { print_serve(&r); }
        }
        Err(e) => { eprintln!("research: {}", e); return 1; }
    }
    if !watch { return 0; }
    let tick = num_opt(parsed, "tick", research::DEFAULT_SCHED_TICK_SECS);
    println!("research --watch: \"{}\" tick={}s（Ctrl+C 结束；期间到期任务自动刷新并写 RAG）", spec.query, tick);
    let store = Arc::new(Mutex::new(s));
    let stop = Arc::new(AtomicBool::new(false));
    let _h = research::spawn_scheduler(store, ro, tick, stop, Box::new(|m| println!("{}", m)));
    loop { std::thread::sleep(std::time::Duration::from_secs(3600)); }
}

pub fn topics(parsed: &Parsed) -> i32 {
    let sub = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "list" };
    if parsed.has_flag("help") || parsed.has_flag("h") || sub == "help" {
        usage();
        return 0;
    }
    let json = parsed.has_flag("json");
    let mut s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("topics: {}", e); return 1; }
    };
    if sub == "list" {
        let topics = s.state().topics.clone();
        if json {
            println!("{}", serde_json::to_string_pretty(&topics).unwrap_or_default());
            return 0;
        }
        println!("topics: {} 个任务 (state={})", topics.len(), s.state_path());
        for t in &topics { print_topic(t); }
        if topics.is_empty() {
            println!("  （空：用 C96 research --query \"关键词\" 建立第一个订阅任务）");
        }
        return 0;
    }
    if sub == "due" {
        let due = research::due_keys(&s, now_ms());
        println!("topics due: {} 个", due.len());
        for k in &due { println!("  {}", k); }
        return 0;
    }
    let q = query_arg(parsed);
    if q.trim().is_empty() {
        eprintln!("topics {}: QUERY is required", sub);
        usage();
        return 1;
    }
    if sub == "show" {
        match state::find_topic(s.state(), &q) {
            Some(i) => { print_topic(&s.state().topics[i]); return 0; }
            None => { eprintln!("topics show: no such topic: {}", q); return 1; }
        }
    }
    if sub == "run" {
        match research::refresh(&mut s, &q, &run_opts(parsed), now_ms()) {
            Ok(r) => {
                if json { println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default()); }
                else { println!("topics run \"{}\":", q); print_run(&r); }
                return 0;
            }
            Err(e) => { eprintln!("topics run: {}", e); return 1; }
        }
    }
    if sub == "rm" || sub == "remove" {
        let removed = state::remove_topic(s.state_mut(), &q);
        let _ = s.persist_state();
        println!("topics rm \"{}\": {}", q, if removed { "removed" } else { "not found" });
        return if removed { 0 } else { 1 };
    }
    if sub == "pause" || sub == "resume" {
        let pause = sub == "pause";
        let mut found = false;
        if let Some(i) = state::find_topic(s.state(), &q) {
            let now = now_ms();
            let st = s.state_mut();
            st.topics[i].paused = pause;
            if !pause { st.topics[i].next_due_ms = now; }
            found = true;
        }
        let _ = s.persist_state();
        if !found { eprintln!("topics {}: no such topic: {}", sub, q); return 1; }
        println!("topics {}: \"{}\"", sub, q);
        return 0;
    }
    eprintln!("unknown topics subcommand: {}", sub);
    usage();
    return 1;
}
