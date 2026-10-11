//! D3: Baidu 驱动 — SERP(<div class="result|c-container">) 解析 + ClickUrl/from=/mu 解包 + BAIDUID。
//!
//! Baidu 反爬较强：真实 URL 优先从容器属性 mu / data-tools 里拿；次级从 baidu.com/link?url=
//! 的 url/ClickUrl/from 参数解包（percent 或 base64url）；都失败则保留中间链接（resolved=false）。
//! 请求需携带 BAIDUID cookie（见 cookie()）。

use crate::fetch::feed::site_of;
use crate::fetch::html::Dom;
use crate::search::make_candidate;
use crate::search::query::base64_decode;
use crate::search::query::percent_decode;
use crate::search::query::query_param;
use crate::search::Candidate;

/// 生成一个 plausible BAIDUID cookie（服务端会下发自己的；先给客户端一个稳定值）。
pub fn cookie() -> String {
    let nanos: u128 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seeded: u128 = nanos ^ (0x5DEECE66DDEADBEEu64 as u128);
    let hex = format!("{:032X}", seeded & 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFFu128);
    return "BAIDUID=".to_string() + &hex + ":FG=1";
}

/// Baidu SERP HTML → 候选。
pub fn parse_serp(html: &str) -> Vec<Candidate> {
    let dom = Dom::parse(html);
    let mut blocks: Vec<usize> = Vec::new();
    for i in 0..dom.len() {
        if dom.tag(i) == "#text" || dom.tag(i) == "#root" { continue; }
        let cls = dom.class(i).to_lowercase();
        if cls.contains("result") || cls.contains("c-container") {
            blocks.push(i);
        }
    }
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
        let h3 = dom.first_descendant(b, "h3");
        let anchor = match h3 {
            Some(h) => dom.first_descendant(h, "a"),
            None => dom.first_descendant(b, "a"),
        };
        let a = match anchor {
            Some(x) => x,
            None => { continue; }
        };
        let title = dom.text(a).trim().to_string();
        let href = dom.attr(a, "href").unwrap_or_default();
        if title.chars().count() < 2 || href.is_empty() { continue; }
        // "相关推荐 / 大家还在搜" 之类不是自然结果
        if dom.class(b).to_lowercase().contains("recommend") { continue; }
        if href.contains("recommend_list") { continue; }
        let (url, resolved) = resolve_url(&dom, b, a, &href);
        if url.is_empty() { continue; }
        let snippet = snippet_of(&dom, b);
        rank += 1;
        out.push(make_candidate(&title, &url, &snippet, "baidu-serp", rank, resolved));
    }
    return out;
}

fn resolve_url(dom: &Dom, block: usize, anchor: usize, href: &str) -> (String, bool) {
    let mut direct = String::new();
    for name in ["mu", "data-url", "data-mu"] {
        if let Some(v) = dom.attr(block, name) {
            if v.starts_with("http://") || v.starts_with("https://") { direct = v; break; }
        }
        if let Some(v) = dom.attr(anchor, name) {
            if v.starts_with("http://") || v.starts_with("https://") { direct = v; break; }
        }
    }
    if !direct.is_empty() { return (direct, true); }
    for name in ["data-tools", "data-log", "data-click"] {
        if let Some(raw) = dom.attr(block, name) {
            if let Some(u) = json_url(&raw) {
                let fixed = u.replace("\\/", "/");
                return (fixed, true);
            }
        }
    }
    return decode_link(href);
}

/// data-tools / data-click 里的 "url":"http..."。
fn json_url(raw: &str) -> Option<String> {
    for key in ["\"url\"", "\"mu\"", "\"target\""] {
        let idx = match raw.find(key) { Some(i) => i, None => { continue; } };
        let rest: &str = &raw[idx + key.len()..];
        let colon = match rest.find(':') { Some(i) => i, None => { continue; } };
        let after: &str = &rest[colon + 1..];
        let q = match after.find('"') { Some(i) => i, None => { continue; } };
        let start = q + 1;
        let tail: &str = &after[start..];
        let end = match tail.find('"') { Some(i) => i, None => { continue; } };
        let v: &str = &tail[..end];
        if v.starts_with("http://") || v.starts_with("https://") {
            return Some(v.to_string());
        }
    }
    return None;
}

/// baidu 跳转 link → 真实 URL。支持 ClickUrl / url / target / from（percent 或 base64url）。
pub fn decode_link(href: &str) -> (String, bool) {
    let h = href.trim();
    if h.is_empty() { return ("".to_string(), false); }
    if h.starts_with("//") { return ("https:".to_string() + h, true); }
    if h.starts_with("http://") || h.starts_with("https://") {
        let host = site_of(h);
        if !host.ends_with("baidu.com") {
            return (h.to_string(), true);
        }
    }
    for key in ["ClickUrl", "url", "target", "from"] {
        if let Some(v) = query_param(h, key) {
            let dec = percent_decode(&v);
            if dec.starts_with("http://") || dec.starts_with("https://") {
                return (dec, true);
            }
            if let Some(b) = base64_decode(&dec) {
                if b.starts_with("http://") || b.starts_with("https://") {
                    return (b, true);
                }
            }
        }
    }
    return (h.to_string(), false);
}

fn snippet_of(dom: &Dom, b: usize) -> String {
    let mut weak = String::new();
    for d in dom.descendants(b) {
        let cls = dom.class(d).to_lowercase();
        if cls.contains("c-abstract") || cls.contains("content-right") || cls.contains("c-span-last") {
            let t = dom.text(d);
            if t.chars().count() > 10 { return t; }
        }
        if weak.is_empty() && dom.tag(d) == "p" {
            let t = dom.text(d);
            if t.chars().count() > 10 { weak = t; }
        }
    }
    return weak;
}

#[cfg(test)]
mod tests {
    use super::decode_link;
    use super::parse_serp;
    use crate::search::query::base64url_encode;

    #[test]
    fn decode_mu_and_link_params() {
        let (u, r) = decode_link("http://www.baidu.com/link?url=http%3A%2F%2Fexample.com%2Fa");
        assert_eq!(u, "http://example.com/a");
        assert_eq!(r, true);
        let target = "https://example.org/post";
        let link = "https://www.baidu.com/link?url=".to_string() + &base64url_encode(target.as_bytes());
        let (u2, r2) = decode_link(&link);
        assert_eq!(u2, target);
        assert_eq!(r2, true);
        let (u3, r3) = decode_link("https://www.baidu.com/link?url=OPAQUE");
        assert_eq!(r3, false);
        assert_eq!(u3.starts_with("https://www.baidu.com/link"), true);
    }

    #[test]
    fn serp_parses_result_blocks() {
        let html = "<html><body><div id=\"content_left\">\
<div class=\"result c-container\" id=\"1\" mu=\"https://example.com/one\">\
<h3 class=\"t\"><a href=\"http://www.baidu.com/link?url=OPAQUE1\">第一条结果</a></h3>\
<div class=\"c-abstract\">这是第一条结果的摘要，包含足够多的中文内容。</div></div>\
<div class=\"result-op c-container\" id=\"2\" data-tools='{\"title\":\"two\",\"url\":\"https://example.org/two\"}'>\
<h3><a href=\"http://www.baidu.com/link?url=OPAQUE2\">第二条结果</a></h3>\
<span class=\"c-span-last\">第二条结果的摘要文本。</span></div>\
</div></body></html>";
        let c = parse_serp(html);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "https://example.com/one");
        assert_eq!(c[0].resolved, true);
        assert_eq!(c[0].title, "第一条结果");
        assert!(c[0].snippet.contains("第一条结果"));
        assert_eq!(c[1].url, "https://example.org/two");
        assert_eq!(c[1].source, "baidu-serp");
    }
}
