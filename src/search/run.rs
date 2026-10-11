//! 引擎执行（从 D CLI 抽出）：Bing RSS/SERP + Baidu SERP → 候选 + 每引擎报告。
//!
//! CLI \`search\` 与 \`research\` 主题刷新共用这一份反爬/解析逻辑，避免两套实现漂移。
//! 报告（EngineReport）只描述结果，打印由调用方决定（CLI 打一行，主题刷新只记状态）。

use serde::Serialize;

use crate::fetch::http::default_opts;
use crate::fetch::http::fetch;
use crate::search::baidu;
use crate::search::bing;
use crate::search::query;
use crate::search::Candidate;

/// CLI/SERP 抓取用的浏览器 UA（Bing/Baidu 对非浏览器 UA 更激进）。
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0";

#[derive(Clone)]
pub struct RunOpts {
    pub timeout_s: usize,
    pub retries: usize,
    pub cookie: String,
}

impl RunOpts {
    pub fn new() -> RunOpts {
        return RunOpts { timeout_s: 20, retries: 1, cookie: String::new() };
    }
}

#[derive(Clone, Serialize)]
pub struct EngineReport {
    pub engine: String,
    pub kind: String,
    pub status: usize,
    pub count: usize,
    pub error: String,
    pub ok: bool,
}

fn rep_of(engine: &str, kind: &str, status: usize, count: usize, error: &str, ok: bool) -> EngineReport {
    return EngineReport {
        engine: engine.to_string(),
        kind: kind.to_string(),
        status: status,
        count: count,
        error: error.to_string(),
        ok: ok,
    };
}

/// 按 plan.engine 跑引擎（auto = bing + baidu），返回（候选, 每引擎报告）。
pub fn collect(plan: &query::Plan, max: usize, o: &RunOpts) -> (Vec<Candidate>, Vec<EngineReport>) {
    let mut out: Vec<Candidate> = Vec::new();
    let mut reps: Vec<EngineReport> = Vec::new();
    for e in query::engines(&plan.engine) {
        if e == "bing" {
            let mut opt = default_opts();
            opt.user_agent = BROWSER_UA.to_string();
            opt.accept = "application/rss+xml, application/atom+xml, application/xml;q=0.9, text/html;q=0.9, */*;q=0.5".to_string();
            opt.timeout_s = o.timeout_s;
            opt.retries = o.retries;
            opt.max_body = 2 * 1024 * 1024;
            let rss_url = query::bing_rss_url(&plan.query, max);
            let rep = fetch(&rss_url, &opt);
            if rep.ok {
                let c = bing::rss_candidates(&rep.body);
                reps.push(rep_of("bing", "rss", rep.status, c.len(), "", true));
                out.extend(c);
            } else {
                reps.push(rep_of("bing", "rss", rep.status, 0, &rep.error, false));
            }
            let serp_url = query::bing_serp_url(&plan.query, max);
            let rep2 = fetch(&serp_url, &opt);
            if rep2.ok {
                let c = bing::parse_serp(&rep2.body);
                reps.push(rep_of("bing", "serp", rep2.status, c.len(), "", true));
                out.extend(c);
            } else {
                reps.push(rep_of("bing", "serp", rep2.status, 0, &rep2.error, false));
            }
        } else if e == "baidu" {
            let mut opt = default_opts();
            opt.user_agent = BROWSER_UA.to_string();
            opt.accept = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.5".to_string();
            opt.accept_language = "zh-CN,zh;q=0.9,en;q=0.5".to_string();
            opt.cookie = if o.cookie.is_empty() { baidu::cookie() } else { o.cookie.clone() };
            opt.timeout_s = o.timeout_s;
            opt.retries = o.retries;
            opt.max_body = 3 * 1024 * 1024;
            let url = query::baidu_serp_url(&plan.query, max * 2);
            let rep = fetch(&url, &opt);
            if rep.ok {
                let c = baidu::parse_serp(&rep.body);
                reps.push(rep_of("baidu", "serp", rep.status, c.len(), "", true));
                out.extend(c);
            } else {
                reps.push(rep_of("baidu", "serp", rep.status, 0, &rep.error, false));
            }
        }
    }
    return (out, reps);
}
