//! D4: URL normalize + hash + 站点白/黑名单 + 候选去重。

use crate::fetch::feed::site_of;
use crate::search::Candidate;

/// 常见跟踪参数（normalize 时丢弃）。
const TRACKING: [&str; 18] = [
    "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content",
    "fbclid", "gclid", "msclkid", "spm", "from", "ref", "ref_src",
    "share_source", "share_medium", "mc_cid", "mc_eid", "_hsenc", "igshid",
];

/// URL → 去 fragment / 去跟踪参数 / host 小写 / 去默认端口 / 去尾部 '/' 的规范形。
pub fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.is_empty() { return "".to_string(); }
    let (scheme, rest) = match u.find("://") {
        Some(i) => (u[..i].to_lowercase(), &u[i + 3..]),
        None => ("".to_string(), u),
    };
    let no_frag: &str = match rest.find('#') {
        Some(i) => &rest[..i],
        None => rest,
    };
    let (authority, tail) = match no_frag.find(|c| c == '/' || c == '?') {
        Some(i) => (&no_frag[..i], &no_frag[i..]),
        None => (no_frag, ""),
    };
    let mut host = authority.to_lowercase();
    if scheme == "http" && host.ends_with(":80") { host.truncate(host.len() - 3); }
    if scheme == "https" && host.ends_with(":443") { host.truncate(host.len() - 4); }
    let (path, query) = match tail.find('?') {
        Some(i) => (&tail[..i], &tail[i + 1..]),
        None => (tail, ""),
    };
    let mut p = path.to_string();
    while p.ends_with('/') { p.pop(); }
    let mut kept: Vec<String> = Vec::new();
    for part in query.split('&') {
        if part.is_empty() { continue; }
        let k = match part.find('=') { Some(i) => &part[..i], None => part };
        let kl = k.to_lowercase();
        let mut drop = false;
        for t in TRACKING {
            if kl == t { drop = true; break; }
        }
        if !drop { kept.push(part.to_string()); }
    }
    let mut out = String::new();
    if !scheme.is_empty() { out = out + &scheme + "://"; }
    out = out + &host + &p;
    if !kept.is_empty() { out = out + "?" + &kept.join("&"); }
    return out;
}

/// FNV-1a 64 → 16 hex（去重键）。
pub fn url_hash(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325u64;
    for b in url.as_bytes() {
        h = h ^ (*b as u64);
        h = h.wrapping_mul(0x100000001b3u64);
    }
    return format!("{:016x}", h);
}

/// 通配符匹配：'*' 任意序列，'?' 单字符。
pub fn match_glob(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut pi: usize = 0;
    let mut ti: usize = 0;
    let mut star: usize = usize::MAX;
    let mut mark: usize = 0;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    return pi == p.len();
}

/// 黑名单命中：pattern 含 '/' 或 '://' → 匹配整个 URL，否则匹配 host（支持 *.x.com）。
pub fn is_blacklisted(url: &str, patterns: &[String]) -> bool {
    let host = site_of(url).to_lowercase();
    let full = url.to_lowercase();
    for pat in patterns {
        let p = pat.trim().to_lowercase();
        if p.is_empty() { continue; }
        if p.contains('/') || p.contains("://") {
            if match_glob(&p, &full) { return true; }
        } else {
            if match_glob(&p, &host) { return true; }
            if !p.contains('*') && host.ends_with(&p) { return true; }
        }
    }
    return false;
}

/// 白名单：空 = 全通过；site 支持 host / *.host / *。
pub fn is_whitelisted(host: &str, sites: &[String]) -> bool {
    if sites.is_empty() { return true; }
    let h = host.to_lowercase();
    for s in sites {
        let p = s.trim().to_lowercase();
        if p.is_empty() { continue; }
        if p == "*" { return true; }
        if match_glob(&p, &h) { return true; }
        if !p.contains('*') && (h == p || h.ends_with(&(".".to_string() + &p))) { return true; }
    }
    return false;
}

/// 去重：按 normalized url；同 key 时 resolved 的真实链接替换未解析的。
pub fn dedup(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for c in candidates {
        let key = url_hash(&normalize_url(&c.url));
        let mut found: Option<usize> = None;
        for (i, k) in keys.iter().enumerate() {
            if *k == key && !key.is_empty() { found = Some(i); break; }
        }
        match found {
            Some(i) => {
                if !out[i].resolved && c.resolved {
                    out[i].url = c.url.clone();
                    out[i].site = c.site.clone();
                    out[i].resolved = true;
                }
            }
            None => {
                keys.push(key);
                out.push(c);
            }
        }
    }
    return out;
}

#[cfg(test)]
mod tests {
    use super::dedup;
    use super::is_blacklisted;
    use super::is_whitelisted;
    use super::match_glob;
    use super::normalize_url;
    use super::url_hash;
    use crate::search::make_candidate;

    #[test]
    fn normalize_strips_tracking_and_fragment() {
        let got = normalize_url("HTTPS://Example.COM:443/a/b/?utm_source=x&id=7#frag");
        assert_eq!(got, "https://example.com/a/b?id=7");
        assert_eq!(normalize_url("http://example.com:80/"), "http://example.com");
    }

    #[test]
    fn hash_is_stable_and_different() {
        let a = url_hash("https://example.com/a");
        let b = url_hash("https://example.com/a");
        let c = url_hash("https://example.com/b");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_ne!(a, c);
    }

    #[test]
    fn glob_and_lists() {
        assert_eq!(match_glob("*.example.com", "news.example.com"), true);
        assert_eq!(match_glob("*.example.com", "example.com"), false);
        assert_eq!(match_glob("*", "anything"), true);
        let bl = vec!["ads.example.com".to_string(), "*.tracker.net".to_string()];
        assert_eq!(is_blacklisted("https://ads.example.com/x", &bl), true);
        assert_eq!(is_blacklisted("https://a.tracker.net/x", &bl), true);
        assert_eq!(is_blacklisted("https://good.com/x", &bl), false);
        let wl: Vec<String> = Vec::new();
        assert_eq!(is_whitelisted("anything.com", &wl), true);
        let wl2 = vec!["example.com".to_string()];
        assert_eq!(is_whitelisted("example.com", &wl2), true);
        assert_eq!(is_whitelisted("news.example.com", &wl2), true);
        assert_eq!(is_whitelisted("evil.com", &wl2), false);
    }

    #[test]
    fn dedup_prefers_resolved() {
        let a = make_candidate("t", "https://example.com/x", "s", "baidu-serp", 1, false);
        let b = make_candidate("t", "https://example.com/x", "s", "bing-serp", 1, true);
        let c = make_candidate("t2", "https://example.com/x#frag", "s2", "bing-rss", 2, true);
        let out = dedup(vec![a, b, c]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
        assert_eq!(out[0].resolved, true);
    }
}
