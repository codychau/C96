//! D2: Bing 驱动 — News RSS 解析 + SERP(<li class="b_algo">) 解析 + /ck/a 跳转解码。
//!
//! Bing 优先顺序（TASKS 阶段 C/D 反爬策略）：RSS（快、便宜）→ SERP HTML。
//! /ck/a 跳转：query 参数 u = "a1" + base64url(真实 URL)（部分场景直接 base64url）。

use crate::fetch::feed::parse_feed;
use crate::fetch::html::Dom;
use crate::search::make_candidate;
use crate::search::query::base64_decode;
use crate::search::query::percent_decode;
use crate::search::query::query_param;
use crate::search::Candidate;

/// Bing News RSS → 候选（RSS item 的 link 已是真实 URL）。
pub fn rss_candidates(xml: &str) -> Vec<Candidate> {
    let feed = match parse_feed(xml) {
        Ok(f) => f,
        Err(_) => { return Vec::new(); }
    };
    let mut out: Vec<Candidate> = Vec::new();
    let mut rank: usize = 0;
    for a in &feed.items {
        if a.url.is_empty() { continue; }
        rank += 1;
        let (url, resolved) = unwrap(&a.url);
        out.push(make_candidate(&a.title, &url, &a.body, "bing-rss", rank, resolved));
    }
    return out;
}

/// Bing SERP HTML → <li class="b_algo"> 候选。
pub fn parse_serp(html: &str) -> Vec<Candidate> {
    let dom = Dom::parse(html);
    let blocks = dom.by_class("b_algo");
    let mut used: Vec<bool> = vec![false; dom.len()];
    let mut out: Vec<Candidate> = Vec::new();
    let mut rank: usize = 0;
    for b in blocks {
        let mut cur = dom.nodes[b].parent;
        let mut nested = false;
        while let Some(p) = cur {
            if used[p] { nested = true; break; }
            cur = dom.nodes[p].parent;
        }
        if nested { continue; }
        used[b] = true;
        let h2 = dom.first_descendant(b, "h2");
        let anchor = match h2 {
            Some(h) => dom.first_descendant(h, "a"),
            None => dom.first_descendant(b, "a"),
        };
        let a = match anchor {
            Some(x) => x,
            None => { continue; }
        };
        let href = dom.attr(a, "href").unwrap_or_default();
        let title = dom.text(a);
        if title.is_empty() || href.is_empty() || href.starts_with('#') { continue; }
        let (url, resolved) = unwrap(&href);
        if url.is_empty() { continue; }
        let snippet = snippet_of(&dom, b);
        rank += 1;
        out.push(make_candidate(&title, &url, &snippet, "bing-serp", rank, resolved));
    }
    return out;
}

fn snippet_of(dom: &Dom, b: usize) -> String {
    let mut fallback = String::new();
    for d in dom.descendants(b) {
        if dom.class(d).to_lowercase().contains("b_caption") {
            let t = dom.text(d);
            if t.len() > 10 { return t; }
        }
        if fallback.is_empty() && dom.tag(d) == "p" {
            let t = dom.text(d);
            if t.len() > 10 { fallback = t; }
        }
    }
    return fallback;
}

/// href → (url, resolved)；Bing /ck/a 跳转解包，相对协议补全。
pub fn unwrap(href: &str) -> (String, bool) {
    let h = href.trim();
    if h.is_empty() { return ("".to_string(), false); }
    if h.contains("/ck/a") {
        if let Some(u) = decode_ck_a(h) { return (u, true); }
        return (h.to_string(), false);
    }
    if h.starts_with("http://") || h.starts_with("https://") { return (h.to_string(), true); }
    if h.starts_with("//") { return ("https:".to_string() + h, true); }
    return (h.to_string(), false);
}

/// https://www.bing.com/ck/a?...&u=a1<base64url> → 真实 URL。
pub fn decode_ck_a(href: &str) -> Option<String> {
    let raw = match query_param(href, "u") {
        Some(v) => v,
        None => { return None; }
    };
    let dec = percent_decode(&raw);
    if dec.starts_with("http://") || dec.starts_with("https://") { return Some(dec); }
    let mut cands: Vec<String> = Vec::new();
    if dec.len() > 2 && dec.starts_with("a1") {
        cands.push(dec[2..].to_string());
    }
    cands.push(dec);
    for c in cands {
        if let Some(url) = base64_decode(&c) {
            if url.starts_with("http://") || url.starts_with("https://") {
                return Some(url);
            }
        }
    }
    return None;
}

#[cfg(test)]
mod tests {
    use super::decode_ck_a;
    use super::parse_serp;
    use super::rss_candidates;
    use crate::search::query::base64_decode;
    use crate::search::query::base64url_encode;

    #[test]
    fn base64_round_trip_variants() {
        assert_eq!(base64_decode("aHR0cHM6Ly9leGFtcGxlLmNvbQ").unwrap(), "https://example.com");
        assert_eq!(base64_decode("aHR0cHM6Ly9leGFtcGxlLmNvbQ==").unwrap(), "https://example.com");
        assert_eq!(base64_decode("!!!").is_none(), true);
    }

    #[test]
    fn ck_a_decodes_base64url_payload() {
        let target = "https://example.com/a b?x=1";
        let payload = "a1".to_string() + &base64url_encode(target.as_bytes());
        let href = format!("https://www.bing.com/ck/a?!&&p=abc&u={}&ntb=1", payload);
        assert_eq!(decode_ck_a(&href).unwrap(), target);
    }

    #[test]
    fn serp_parses_b_algo() {
        let payload = "a1".to_string() + &base64url_encode(b"https://example.org/two");
        let head = "<html><body><ol id=\"b_results\">\
<li class=\"b_algo\"><h2><a href=\"https://example.com/one\">First <b>Result</b></a></h2>\
<div class=\"b_caption\"><p>A snippet for the first result &amp; more.</p></div></li>\
<li class=\"b_algo b_algoBorder\"><h2><a href=\"https://www.bing.com/ck/a?u=";
        let tail = "\">Second</a></h2>\
<p>Second snippet text here.</p></li>\
<li class=\"b_ans\"><a href=\"https://ignore.me\">not a result</a></li>\
</ol></body></html>";
        let html = head.to_string() + &payload + tail;
        let c = parse_serp(&html);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "https://example.com/one");
        assert_eq!(c[0].title, "First Result");
        assert!(c[0].snippet.contains("first result"));
        assert_eq!(c[1].url, "https://example.org/two");
        assert_eq!(c[1].source, "bing-serp");
    }

    #[test]
    fn rss_candidates_from_feed() {
        let xml = "<rss version=\"2.0\"><channel><title>n</title>\
<item><title>One</title><link>https://example.com/1</link><description>d1</description></item>\
<item><title>Two</title><link>https://example.com/2</link><description>d2</description></item>\
</channel></rss>";
        let c = rss_candidates(xml);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].source, "bing-rss");
        assert_eq!(c[1].rank, 2);
    }
}
