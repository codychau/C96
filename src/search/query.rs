//! D1: query builder — 用户偏好（keywords × site × engine）→ 引擎 URL。
//!
//! 搜索引擎无关的百分比编解码 + base64url + query param 工具也放这里（Bing/Baidu 共用）。

use crate::config::pref::Feed;

pub struct Plan {
    pub engine: String,
    pub query: String,
    pub site: String,
    pub keywords: Vec<String>,
}

fn unreserved(b: u8) -> bool {
    return (b >= b'A' && b <= b'Z') || (b >= b'a' && b <= b'z') || (b >= b'0' && b <= b'9')
        || b == b'-' || b == b'_' || b == b'.' || b == b'~';
}

/// UTF-8 → application/x-www-form-urlencoded（空格 = %20）。
pub fn percent_encode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let hex = b"0123456789ABCDEF";
    for b in bytes {
        if unreserved(*b) {
            out.push(*b as char);
        } else {
            out.push('%');
            out.push(hex[(*b >> 4) as usize] as char);
            out.push(hex[(*b & 0x0F) as usize] as char);
        }
    }
    return out;
}

/// %XX（含 '+' → space）→ UTF-8 lossy。
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < n {
        let b = bytes[i];
        if b == b'%' && i + 2 < n {
            let h = hex_val(bytes[i + 1]);
            let l = hex_val(bytes[i + 2]);
            if h >= 0 && l >= 0 {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        if b == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(b);
        i += 1;
    }
    return String::from_utf8_lossy(&out).to_string();
}

fn hex_val(b: u8) -> i32 {
    if b >= b'0' && b <= b'9' { return (b - b'0') as i32; }
    if b >= b'a' && b <= b'f' { return (b - b'a') as i32 + 10; }
    if b >= b'A' && b <= b'F' { return (b - b'A') as i32 + 10; }
    return -1;
}

/// base64 / base64url 解码（容忍 '=' padding；Bing /ck/a 的 u= 参数）。
pub fn base64_decode(s: &str) -> Option<String> {
    let mut out: Vec<u8> = Vec::new();
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for ch in s.chars() {
        let v: u32 = match ch {
            'A'..='Z' => (ch as u32) - ('A' as u32),
            'a'..='z' => (ch as u32) - ('a' as u32) + 26,
            '0'..='9' => (ch as u32) - ('0' as u32) + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            '=' => { continue; }
            _ => { return None; }
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    return String::from_utf8(out).ok();
}

/// base64 编码（标准字母表 + '=' padding）——测试专用。
#[cfg(test)]
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i: usize = 0;
    while i + 2 < data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = (data[i] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    return out;
}

/// base64url 编码（'-' '_'，无 padding）——Bing /ck/a 的 u= 参数（测试构造用）。
#[cfg(test)]
pub fn base64url_encode(data: &[u8]) -> String {
    let s = base64_encode(data);
    let mut out = String::new();
    for c in s.chars() {
        if c == '=' { continue; }
        if c == '+' { out.push('-'); continue; }
        if c == '/' { out.push('_'); continue; }
        out.push(c);
    }
    return out;
}

/// url 的 query 参数（第一个匹配），值保持原始 percent 编码。
pub fn query_param(url: &str, key: &str) -> Option<String> {
    let start = match url.find('?') { Some(i) => i + 1, None => { return None; } };
    let q: &str = &url[start..];
    let end = match q.find('#') { Some(i) => i, None => q.len() };
    let qs: &str = &q[..end];
    for part in qs.split('&') {
        if part.is_empty() { continue; }
        let (k, v) = match part.find('=') {
            Some(i) => (&part[..i], &part[i + 1..]),
            None => (part, ""),
        };
        if k == key {
            return Some(v.to_string());
        }
    }
    return None;
}

/// auto → [bing, baidu]；其余按用户指定单引擎。
pub fn engines(engine: &str) -> Vec<String> {
    let e = engine.trim().to_lowercase();
    if e == "" || e == "auto" || e == "all" {
        let mut v: Vec<String> = Vec::new();
        v.push("bing".to_string());
        v.push("baidu".to_string());
        return v;
    }
    let mut v: Vec<String> = Vec::new();
    v.push(e);
    return v;
}

/// keywords × site → 最终 query 文本（site:host 语法）。
pub fn build_plan(keywords: &[String], site: &str, engine: &str) -> Plan {
    let mut q = keywords.join(" ");
    let s = site.trim();
    if !s.is_empty() {
        if q.is_empty() {
            q = "site:".to_string() + s;
        } else {
            q = q + " site:" + s;
        }
    }
    return Plan {
        engine: engine.trim().to_lowercase(),
        query: q,
        site: s.to_string(),
        keywords: keywords.to_vec(),
    };
}

/// 从 [[feeds]] 偏好直接生成 plan（D1：keyword × site × engine 限定）。
pub fn plan_from_feed(f: &Feed) -> Plan {
    return build_plan(&f.keywords, &f.site, &f.engine);
}

pub fn bing_rss_url(q: &str, count: usize) -> String {
    return format!("https://www.bing.com/news/search?q={}&format=rss&count={}&setmkt=zh-CN",
        percent_encode(q), count);
}

pub fn bing_serp_url(q: &str, count: usize) -> String {
    return format!("https://www.bing.com/search?q={}&count={}&setlang=zh-CN&form=QBLH",
        percent_encode(q), count);
}

pub fn baidu_serp_url(q: &str, rn: usize) -> String {
    return format!("https://www.baidu.com/s?wd={}&rn={}&ie=utf-8", percent_encode(q), rn);
}
#[cfg(test)]
mod tests {
    use super::baidu_serp_url;
    use super::bing_rss_url;
    use super::bing_serp_url;
    use super::build_plan;
    use super::engines;
    use super::percent_decode;
    use super::percent_encode;
    use super::query_param;

    #[test]
    fn percent_round_trip() {
        assert_eq!(percent_encode("rust 编译器"), "rust%20%E7%BC%96%E8%AF%91%E5%99%A8");
        assert_eq!(percent_decode("rust%20%E7%BC%96%E8%AF%91%E5%99%A8"), "rust 编译器");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn plan_adds_site_and_engine() {
        let kw = vec!["rust".to_string(), "编译器".to_string()];
        let p = build_plan(&kw, "example.com", "auto");
        assert_eq!(p.query, "rust 编译器 site:example.com");
        assert_eq!(p.site, "example.com");
        assert_eq!(p.keywords.len(), 2);
        assert_eq!(engines("auto").len(), 2);
        assert_eq!(engines("bing"), vec!["bing".to_string()]);
        assert_eq!(engines("").len(), 2);
        assert_eq!(build_plan(&kw, "", "bing").query, "rust 编译器");
    }

    #[test]
    fn engine_urls_encode_query() {
        let u = bing_serp_url("rust 编译器", 20);
        assert!(u.starts_with("https://www.bing.com/search?q=rust%20%E7%BC%96"));
        assert!(u.contains("count=20"));
        assert!(bing_rss_url("x", 5).contains("format=rss"));
        assert!(baidu_serp_url("a&b", 10).contains("wd=a%26b"));
        assert!(baidu_serp_url("a&b", 10).contains("rn=10"));
    }

    #[test]
    fn query_param_extracts_first() {
        assert_eq!(query_param("https://x/?a=1&u=abc", "u").unwrap(), "abc");
        assert_eq!(query_param("https://x/?a=1", "u"), None);
        assert_eq!(query_param("https://x/", "u"), None);
    }
}
