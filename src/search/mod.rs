//! 阶段 D — 搜索引擎驱动层（Bing / Baidu SERP）。
//!   D1 query ：偏好（keyword × site × engine）→ 引擎查询 URL
//!   D2 bing  ：News RSS 解析 + SERP(<li class="b_algo">) 解析 + /ck/a 跳转解码
//!   D3 baidu ：SERP(<div class="result|c-container">) 解析 + ClickUrl/from=/mu 解包 + BAIDUID cookie
//!   D4 norm  ：URL normalize + hash + 站点白/黑名单 + 去重

pub mod query;
pub mod run;
pub mod bing;
pub mod baidu;
pub mod normalize;

use crate::fetch::feed::site_of;

/// 一个搜索结果候选（统一 Bing/Baidu/RSS 形状）。
#[derive(Clone)]
pub struct Candidate {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
    pub site: String,
    pub rank: usize,
    /// url 是否已解析为真实外链（false = 仍是百度跳转等中间链接）
    pub resolved: bool,
}

pub fn make_candidate(title: &str, url: &str, snippet: &str, source: &str, rank: usize, resolved: bool) -> Candidate {
    let site = site_of(url);
    return Candidate {
        title: title.to_string(),
        url: url.to_string(),
        snippet: snippet.to_string(),
        source: source.to_string(),
        site: site,
        rank: rank,
        resolved: resolved,
    };
}

