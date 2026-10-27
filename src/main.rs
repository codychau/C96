//! C96 — 资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库 (RAG)
//!
//! 阶段 A 骨架：验证 toolchain + 目录布局，零外部依赖（可离线编译）.
//! 阶段 B 偏好/配置层：config/feeds.toml + 手写 TOML subset parse + feeds list/add.
//! 后续阶段逐步填充 src/ 模块，每个阶段独立可编译、可验证：
//!   B 偏好/配置 · C 轻量抓取(RSS) · D 搜索引擎 SERP · E 浏览器自控 · F 存储
//!   G 索引(BM25) · H AI/RAG · I 订阅服务 · J 测试/性能/文档

pub use c96::config;
pub use c96::fetch;
pub use c96::index;
pub use c96::research;
pub use c96::rss;
pub use c96::search;
pub use c96::store;

mod cli;

const PROJECT_NAME: &str = "C96";
const TAGLINE: &str = "资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库";
const DEFAULT_CONFIG: &str = "config/feeds.toml";
const VERSION_LINE: &str = "C96 0.1.0 — rustc 1.97.0 / cargo 1.97.0, zero-dependency core (offline build)";

/// 人类可读 banner.
fn banner() -> String {
    return format!("{} — {}", PROJECT_NAME, TAGLINE);
}

/// usage text（阶段 B 子命令）.
fn usage() {
    println!("{}", banner());
    println!("usage: C96 <command> [options]");
    println!();
    println!("commands:");
    println!("  feeds list [--config PATH]        print user preferences (defaults + feeds)");
    println!("  feeds add --url URL [options]     append one [[feeds]] block to the config file");
    println!("  fetch --url URL           one HTTP GET: UA/timeout/retry + ETag cache (C1)");
    println!("  feed parse --file PATH [--limit N]   parse a local RSS 2.0 / Atom feed (C2)");
    println!("  html extract --file PATH          extract main content from a local HTML page (C3)");
    println!("  search --query Q [options]        Bing/Baidu SERP → normalized candidates (D)");
    println!("  store stats|list|search|purge|add|state    RAG knowledge base (JSONL, F)");
    println!("  research --query Q [--watch]      topic subscription: search→RAG→refresh (I)");
    println!("  topics list|show|run|pause|resume|rm|due   persistent topic task list (I)");
    println!("  index stats|query|bench            BM25 index: char-code terms + weights (G)");
    println!("  rss serve|urls|opml                local RSS server: topics -> feeds (I3/I4)");
    println!("  gui                              launch the Tauri GUI (double-click / no args does this too)");
    println!("  gui --silent                     silent mode: start hidden (no window); if already running, do NOT raise it");
    println!("  banner                            print project banner");
    println!("  help | --help                     this text");
    println!("  --version                         version + toolchain");
    println!();
    println!("feeds add options:");
    println!("  --url URL             required, http(s):// feed or SERP entry");
    println!("  --engine bing|baidu|auto          default: auto");
    println!("  --keywords a,b        comma separated search keywords");
    println!("  --site HOST           site restriction (optional)");
    println!("  --weight N            0 < N <= 10, default 1.0");
    println!("  --max-age-hours N     drop articles older than N hours, default 48");
    println!("  --blacklist a,b       host/URL glob patterns (* allowed)");
    println!("  --name NAME           label; derived from url host when omitted");
    println!("  --note TEXT           free-form note");
    println!("  --disabled            store the feed but skip it on refresh");
    println!("  --dry-run             print the block instead of writing the file");
    println!("  --force               allow a duplicate url");
    println!();
    println!("fetch options:");
    println!("  --url URL             required, http(s)://");
    println!("  --timeout SECS        overall timeout, default 20");
    println!("  --connect SECS        connect timeout, default 6");
    println!("  --retries N           extra attempts on transport error, default 2");
    println!("  --max-body N          body cap in bytes, default 1048576");
    println!("  --cache PATH          ETag/Last-Modified cache file, default cache/http_cache.toml");
    println!("  --no-cache            bypass the cache entirely");
    println!("  --show-body           print the first 400 chars of the body");
    println!();
    println!("store options (F):");
    println!("  --kb PATH             knowledge base JSONL, default %APPDATA%/com.c96.gui/kb.jsonl");
    println!("  --ttl-days N          drop RAG docs older than N days, default 30");
    println!("  store add --url URL [--title T] [--text T | --text-file PATH] [--query Q]");
    println!("  --source S            origin label, default manual");
    println!("  --backdate-days N     store add: backdate stored_at_ms (TTL verification)");
    println!("  --store               fetch / feed parse / html extract / search: also write into the KB");
    println!();
    println!("research / topics (I):");
    println!("  research --query Q [--engine auto|bing|baidu] [--max N] [--fetch N]");
    println!("  --interval SECS       refresh interval, default 1800 (30 min)");
    println!("  --fresh SECS          serve from RAG within this window, default 21600 (6 h)");
    println!("  --json / --cache-only / --watch [--tick SECS]");
    println!("  topics list|show|run|pause|resume|rm|due  — 任务清单（持久化在 <kb>.state.toml 的 [[topic]]）");
    println!();
    println!("index (G，精简版):");
    println!("  index stats [--kb PATH] [--json]");
    println!("  index query --query Q [--limit N] [--json]     词项=字符码点整数，标题x3/正文x1，BM25 + 短语加成");
    println!("  index bench [--docs N] [--queries N]           合成 10k 文档压测（验收 <20ms/query）");
    println!();
    println!("rss (I3/I4，本地 RSS 服务器):");
    println!("  rss serve [--bind 127.0.0.1:8787] [--tick SECS] [--no-refresh]   常驻服务 + 关键词调度");
    println!("  rss urls  /  rss opml                  列出订阅地址 / 打印 OPML");
    println!("  endpoints: /  /rss/all.xml  /rss/t/<slug>.xml  /rss/topic.xml?q=Q  /rss/source/<name>.xml  /opml.xml");
    println!("  新入库文档 = 新 item（guid=内容 hash，pubDate=stored_at_ms）-> 阅读器轮询即见推送");
    println!();
    println!("config file default: {} (override with --config PATH)", DEFAULT_CONFIG);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = cli::args::parse(argv);
    let cmd: &str = if parsed.positional.len() > 0 { parsed.positional[0].as_str() } else { "" };
    if parsed.has_flag("version") {
        println!("{}", VERSION_LINE);
        std::process::exit(0);
    }
    if cmd == "" && !parsed.has_flag("help") && !parsed.has_flag("h") {
        // 双击（无参数）：优先拉起 GUI 进程；GUI 未构建时回退到 usage。
        match cli::gui::launch(false) {
            Ok(p) => {
                println!("C96 GUI 已启动：{}", p.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[C96] {}", e);
                usage();
                std::process::exit(0);
            }
        }
    }
    if cmd == "gui" {
        let silent = parsed.has_flag("silent") || parsed.has_flag("hidden");
        match cli::gui::launch(silent) {
            Ok(p) => {
                if silent {
                    println!("C96 GUI 静默启动：{}（窗口隐藏；若已在运行则不唤窗）", p.display());
                } else {
                    println!("C96 GUI 已启动：{}", p.display());
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[C96] {}", e);
                std::process::exit(1);
            }
        }
    }
    if cmd == "help" || parsed.has_flag("help") || parsed.has_flag("h") {
        usage();
        std::process::exit(0);
    }
    if cmd == "banner" {
        println!("{}", banner());
        std::process::exit(0);
    }
    // 便携解析：CWD 没有就找 exe 同级的 config/（换目录安装后从别处调用也能用）
    let config_path = parsed.opt_or("config", &cli::paths::resolve_default(DEFAULT_CONFIG));
    if cmd == "feeds" {
        let sub: &str = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "" };
        if sub == "list" {
            std::process::exit(cli::feeds::list(&config_path));
        }
        if sub == "add" {
            std::process::exit(cli::feeds::add(&config_path, &parsed));
        }
        eprintln!("unknown feeds subcommand: {}", sub);
        usage();
        std::process::exit(1);
    }
    if cmd == "fetch" {
        std::process::exit(cli::http::run(&parsed));
    }
    if cmd == "feed" {
        let sub: &str = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "" };
        if sub == "parse" {
            std::process::exit(cli::feed::run(&parsed));
        }
        eprintln!("unknown feed subcommand: {}", sub);
        usage();
        std::process::exit(1);
    }
    if cmd == "html" {
        let sub: &str = if parsed.positional.len() > 1 { parsed.positional[1].as_str() } else { "" };
        if sub == "extract" {
            std::process::exit(cli::html::run(&parsed));
        }
        eprintln!("unknown html subcommand: {}", sub);
        usage();
        std::process::exit(1);
    }
    if cmd == "search" {
        std::process::exit(cli::search::run(&parsed));
    }
    if cmd == "store" {
        std::process::exit(cli::store::run(&parsed));
    }
    if cmd == "research" {
        std::process::exit(cli::topic::run(&parsed));
    }
    if cmd == "topics" {
        std::process::exit(cli::topic::topics(&parsed));
    }
    if cmd == "index" {
        std::process::exit(cli::index::run(&parsed));
    }
    if cmd == "rss" {
        std::process::exit(cli::rss::run(&parsed));
    }
    eprintln!("unknown command: {}", cmd);
    usage();
    std::process::exit(1);
}
