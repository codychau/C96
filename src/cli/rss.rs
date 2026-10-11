//! 阶段 I3/I4 CLI：\`C96 rss serve|opml|urls\` —— 把常驻进程变成本地 RSS 服务器。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use crate::cli::args::Parsed;
use crate::cli::store::kb_path;
use crate::cli::store::ttl_days;
use crate::research;
use crate::rss;
use crate::store::kb::Store;

fn usage() {
    println!("usage: C96 rss <subcommand> [options]");
    println!("  rss serve [--bind ADDR] [--kb PATH] [--tick SECS] [--no-refresh] [--token X]");
    println!("      启动本地 RSS 服务器（常驻）：关键词有新搜索结果 -> 新条目推给阅读器");
    println!("  rss urls [--bind ADDR] [--kb PATH]     列出全部订阅地址");
    println!("  rss opml [--bind ADDR] [--kb PATH]     打印 OPML（阅读器一键导入）");
    println!();
    println!("  端点: /  ·  /rss/all.xml  ·  /rss/t/<slug>.xml  ·  /rss/topic.xml?q=关键词");
    println!("        /rss/source/<name>.xml  ·  /opml.xml  ·  /health");
    println!("  --bind 默认 {}", rss::DEFAULT_BIND);
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

/// 只用显式 --token / 环境变量（不自动生成；urls/opml 要能对上正在跑的服务器）。
fn given_token(parsed: &Parsed) -> String {
    let explicit = parsed.opt_or("token", "");
    if !explicit.is_empty() { return explicit; }
    return std::env::var("C96_RSS_TOKEN").unwrap_or_default();
}

/// 默认**不设 token**：地址干净，阅读器直接添加。要限制访问才显式 --token / C96_RSS_TOKEN。
fn serve_token(parsed: &Parsed) -> String {
    if parsed.has_flag("no-token") { return String::new(); }
    return given_token(parsed);
}

fn run_opts() -> research::RunOpts {
    let mut o = research::RunOpts::new();
    o.net.timeout_s = 20;
    o.net.retries = 1;
    return o;
}

pub fn run(parsed: &Parsed) -> i32 {
    let sub = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "urls" };
    if parsed.has_flag("help") || parsed.has_flag("h") || sub == "help" {
        usage();
        return 0;
    }
    let bind = parsed.opt_or("bind", rss::DEFAULT_BIND);
    if sub == "serve" { return serve(parsed, &bind); }
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("rss {}: {}", sub, e); return 1; }
    };
    let adv = rss::advertised(&bind);
    let tok = given_token(parsed);
    if sub == "opml" {
        print!("{}", rss::render_opml(&adv, &s.state().topics, &tok));
        return 0;
    }
    if sub == "urls" || sub == "list" {
        println!("RSS 服务地址（监听 {}，对外 {}，kb={}）：", bind, adv, kb_path(parsed));
        println!("  {}", rss::tokenize(&format!("http://{}/rss/all.xml", adv), &tok));
        for t in s.state().topics.iter() {
            println!("  {}   关键词「{}」 runs={} 间隔 {}s 新鲜 {}s",
                rss::tokenize(&format!("http://{}{}", adv, rss::topic_path(t)), &tok),
                t.query, t.runs, t.interval_secs, t.fresh_secs);
        }
        println!("  {}", rss::tokenize(&format!("http://{}/opml.xml", adv), &tok));
        for name in s.list(200, "").iter().map(|d| d.source.clone()).filter(|n| !n.is_empty()).collect::<std::collections::BTreeSet<String>>() {
            println!("  {}   来源 {}", rss::tokenize(&format!("http://{}/rss/source/{}.xml", adv, name), &tok), name);
        }
        if tok.is_empty() && !rss::is_loopback(&bind) {
            println!("  （局域网无 token 直连：阅读器直接添加即可；要限制访问就 --token X 或 C96_RSS_TOKEN）");
        }
        println!("  提示：先跑 C96 rss serve 或打开 GUI，这些地址才可访问；关键词任务用 C96 research --query Q 建立。");
        return 0;
    }
    eprintln!("unknown rss subcommand: {}", sub);
    usage();
    return 1;
}

fn serve(parsed: &Parsed, bind: &str) -> i32 {
    let s = match open(parsed) {
        Ok(s) => s,
        Err(e) => { eprintln!("rss serve: {}", e); return 1; }
    };
    let topics = s.state().topics.len();
    let docs = s.stats().docs;
    let token = serve_token(parsed);
    let arc = Arc::new(Mutex::new(s));
    let local = match rss::spawn_server(arc.clone(), bind, &token) {
        Ok(l) => l,
        Err(e) => { eprintln!("rss serve: {}", e); return 1; }
    };
    println!("[c96 rss] kb={} docs={} topics={}", kb_path(parsed), docs, topics);
    if !parsed.has_flag("no-refresh") {
        let tick = num_opt(parsed, "tick", research::DEFAULT_SCHED_TICK_SECS);
        let stop = Arc::new(AtomicBool::new(false));
        let _h = research::spawn_scheduler(arc, run_opts(), tick, stop, Box::new(|m| println!("{}", m)));
        println!("[c96 rss] 关键词调度已启动（tick={}s）：搜索到新结果就入库，对应 feed 立刻多一条 item", tick);
    }
    println!("[c96 rss] 首页 {}", rss::tokenize(&format!("http://{}/", local), &token));
    println!("[c96 rss] OPML {}", rss::tokenize(&format!("http://{}/opml.xml", local), &token));
    println!("[c96 rss] Ctrl+C 结束");
    loop { std::thread::sleep(std::time::Duration::from_secs(3600)); }
}
