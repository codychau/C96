//! C1: HTTP client — UA / timeout / retry（exponential backoff + Retry-After）/ conditional GET
//!   （ETag + Last-Modified）/ body cap.
//!
//! backend: ureq 2.12.1（rustls + ring; default features = gzip, **no** charset feature →
//!   non-UTF8 bodies decode via from_utf8_lossy; GBK SERP handling belongs to 阶段 D）.
//! measured: 81 pkgs in Cargo.lock, cold build ~8s.
//!
//! live facts 2026-10-07（verified on this machine）:
//!   Bing News RSS / Baidu SERP → **no** ETag, **no** Last-Modified → plain GET every time.
//!   https://example.com/                  → Last-Modified only   → 2nd GET = 304
//!   https://raw.githubusercontent.com/…  → weak ETag W/"…"       → 2nd GET = 304
//!   https://www.rust-lang.org/            → ETag + Last-Modified → 2nd GET = 304
//!
//! ureq turns 4xx/5xx into Error::Status by default; we opt into or_any_status() so an HTTP
//!   status lands in Reply（reported, never retried）while only transport kinds retry.

use ureq::builder;
use ureq::OrAnyStatus;
use ureq::Transport;
use ureq::ErrorKind;
use ureq::Response;
use std::time::Duration;
use std::thread::sleep;
use std::io::Read;

const MAX_BACKOFF_MS: usize = 8000;
const MAX_RETRY_AFTER_S: usize = 30;

#[derive(Clone)]
pub struct Opts {
    pub timeout_s: usize,
    pub connect_s: usize,
    pub retries: usize,
    pub backoff_ms: usize,
    pub max_body: usize,
    pub redirects: usize,
    pub user_agent: String,
    pub accept: String,
    pub accept_language: String,
    pub if_none_match: String,
    pub if_modified_since: String,
    /// 阶段 D：SERP 需要的 Cookie（Baidu BAIDUID 等）；空 = 不发 Cookie.
    pub cookie: String,
}

#[derive(Clone)]
pub struct Reply {
    pub url: String,
    pub final_url: String,
    pub status: usize,
    pub ok: bool,
    pub not_modified: bool,
    pub content_type: String,
    pub etag: String,
    pub last_modified: String,
    pub body: String,
    pub bytes: usize,
    pub truncated: bool,
    pub attempts: usize,
    pub error: String,
}

pub fn default_opts() -> Opts {
    return Opts {
        timeout_s: 20,
        connect_s: 6,
        retries: 2,
        backoff_ms: 400,
        max_body: 1048576,
        redirects: 5,
        user_agent: "C96/0.1.0 rss-aggregator".to_string(),
        accept: "application/rss+xml, application/atom+xml, application/xml;q=0.9, text/html;q=0.7, */*;q=0.5".to_string(),
        accept_language: "zh-CN,zh;q=0.9,en;q=0.8".to_string(),
        if_none_match: "".to_string(),
        if_modified_since: "".to_string(),
        cookie: "".to_string(),
    };
}

/// exponential backoff（ms）before the next attempt; attempt = attempts already made（1-based）.
/// doubles from base, capped at MAX_BACKOFF_MS; a Retry-After hint wins（capped at 30s）.
pub fn backoff_ms(base: usize, attempt: usize, retry_after: usize) -> usize {
    let mut d = base;
    let mut i: usize = 1;
    while i < attempt && d < MAX_BACKOFF_MS {
        d = d * 2;
        i += 1;
    }
    if d > MAX_BACKOFF_MS { d = MAX_BACKOFF_MS; }
    let mut out = d;
    if retry_after > out { out = retry_after; }
    if out > MAX_RETRY_AFTER_S * 1000 { out = MAX_RETRY_AFTER_S * 1000; }
    return out;
}

/// Retry-After header → ms. Only the integer-seconds form is honoured; an HTTP-date → 0
/// （no calendar layer yet — 阶段 F adds one）.
pub fn retry_after_ms(text: &str) -> usize {
    if text.len() == 0 { return 0; }
    let t = text.trim();
    match t.parse::<usize>() {
        Err(_) => { return 0; }
        Ok(s) => {
            if s > MAX_RETRY_AFTER_S { return MAX_RETRY_AFTER_S * 1000; }
            return s * 1000;
        }
    }
}

/// deterministic transport errors（bad url / scheme / redirect loop）never improve on a second try.
pub fn retryable(t: &Transport) -> bool {
    match t.kind() {
        ErrorKind::InvalidUrl => { return false; }
        ErrorKind::UnknownScheme => { return false; }
        ErrorKind::InsecureRequestHttpsOnly => { return false; }
        ErrorKind::TooManyRedirects => { return false; }
        ErrorKind::InvalidProxyUrl => { return false; }
        ErrorKind::ProxyUnauthorized => { return false; }
        _ => { return true; }
    }
}

/// Transport → one line（kind + message）instead of the whole struct dump.
pub fn transport_text(t: &Transport) -> String {
    let kind = format!("{:?}", t.kind());
    match t.message() {
        None => { return kind; }
        Some(m) => { return format!("{}: {}", kind, m.to_string()); }
    }
}

/// header helper（ureq returns Option<&str>）.
pub fn header_of(r: &Response, name: &str) -> String {
    match r.header(name) {
        None => { return "".to_string(); }
        Some(v) => { return v.to_string(); }
    }
}

/// a failed Reply（status 0 = never got an HTTP status）.
pub fn failed_reply(url: &str, attempts: usize, error: &str) -> Reply {
    return Reply {
        url: url.to_string(),
        final_url: "".to_string(),
        status: 0,
        ok: false,
        not_modified: false,
        content_type: "".to_string(),
        etag: "".to_string(),
        last_modified: "".to_string(),
        body: "".to_string(),
        bytes: 0,
        truncated: false,
        attempts: attempts,
        error: error.to_string(),
    };
}

/// one GET with opts; never panics — transport failures land in Reply.error.
pub fn fetch(url: &str, opts: &Opts) -> Reply {
    let agent = builder()
        .user_agent(&opts.user_agent)
        .timeout(Duration::from_secs(opts.timeout_s as u64))
        .timeout_connect(Duration::from_secs(opts.connect_s as u64))
        .timeout_read(Duration::from_secs(opts.timeout_s as u64))
        .redirects(opts.redirects as u32)
        .build();
    let mut attempt: usize = 0;
    let mut last_err = "".to_string();
    let mut retry_after = 0;
    while attempt <= opts.retries {
        if attempt > 0 {
            let wait = backoff_ms(opts.backoff_ms, attempt, retry_after);
            sleep(Duration::from_millis(wait as u64));
        }
        attempt += 1;
        retry_after = 0;
        let mut req = agent.get(url);
        if opts.accept.len() > 0 { req = req.set("Accept", &opts.accept); }
        if opts.accept_language.len() > 0 { req = req.set("Accept-Language", &opts.accept_language); }
        if opts.if_none_match.len() > 0 { req = req.set("If-None-Match", &opts.if_none_match); }
        if opts.if_modified_since.len() > 0 { req = req.set("If-Modified-Since", &opts.if_modified_since); }
        if opts.cookie.len() > 0 { req = req.set("Cookie", &opts.cookie); }
        match req.call().or_any_status() {
            Err(t) => {
                last_err = transport_text(&t);
                if !retryable(&t) {
                    return failed_reply(url, attempt, &last_err);
                }
            },
            Ok(r) => {
                let status: usize = r.status() as usize;
                let final_url = r.get_url().to_string();
                if (status == 429 || status == 503) && attempt <= opts.retries {
                    retry_after = retry_after_ms(&header_of(&r, "retry-after"));
                    last_err = format!("http {} at {}", status, final_url);
                    continue;
                }
                // capture validators before into_reader() consumes r
                let content_type = r.content_type().to_string();
                let etag = header_of(&r, "etag");
                let last_modified = header_of(&r, "last-modified");
                let mut buf: Vec<u8> = Vec::new();
                let want: usize = opts.max_body + 1;
                let mut limited = r.into_reader().take(want as u64);
                let mut read_err = "".to_string();
                match limited.read_to_end(&mut buf) {
                    Err(e) => { read_err = format!("body read: {:?}", e); }
                    Ok(_) => { }
                }
                if read_err.len() > 0 {
                    last_err = read_err;
                    if attempt <= opts.retries { continue; }
                    return failed_reply(url, attempt, &last_err);
                }
                let truncated = buf.len() > opts.max_body;
                if truncated { buf.truncate(opts.max_body); }
                let bytes: usize = buf.len();
                let body = String::from_utf8_lossy(&buf);
                return Reply {
                    url: url.to_string(),
                    final_url: final_url,
                    status: status,
                    ok: (status >= 200 && status < 300) || status == 304,
                    not_modified: status == 304,
                    content_type: content_type,
                    etag: etag,
                    last_modified: last_modified,
                    body: body.to_string(),
                    bytes: bytes,
                    truncated: truncated,
                    attempts: attempt,
                    error: "".to_string(),
                };
            }
        }
    }
    return failed_reply(url, attempt, &last_err);
}

#[cfg(test)]
mod tests {
    use crate::fetch::http::default_opts;
    use crate::fetch::http::fetch;
    use crate::fetch::http::backoff_ms;
    use crate::fetch::http::retry_after_ms;
    use crate::fetch::http::failed_reply;

    #[test]
    fn defaults_are_polite() {
        let o = default_opts();
        assert_eq!(o.timeout_s, 20);
        assert_eq!(o.connect_s, 6);
        assert_eq!(o.retries, 2);
        assert_eq!(o.backoff_ms, 400);
        assert_eq!(o.max_body, 1048576);
        assert_eq!(o.redirects, 5);
        assert!(o.user_agent.starts_with("C96/"));
        assert!(o.accept.contains("application/rss+xml"));
    }

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff_ms(400, 1, 0), 400);
        assert_eq!(backoff_ms(400, 2, 0), 800);
        assert_eq!(backoff_ms(400, 3, 0), 1600);
        assert_eq!(backoff_ms(400, 9, 0), 8000);
        assert_eq!(backoff_ms(0, 3, 0), 0);
    }

    #[test]
    fn retry_after_hint_wins_but_capped() {
        assert_eq!(backoff_ms(400, 1, 25000), 25000);
        assert_eq!(backoff_ms(400, 1, 60000), 30000);
    }

    #[test]
    fn retry_after_seconds_parsed() {
        assert_eq!(retry_after_ms("12"), 12000);
        assert_eq!(retry_after_ms(" 7 "), 7000);
        assert_eq!(retry_after_ms("900"), 30000);
        assert_eq!(retry_after_ms("Wed, 07 Oct 2026 01:00:00 GMT"), 0);
        assert_eq!(retry_after_ms(""), 0);
    }

    #[test]
    fn bad_url_fails_fast() {
        let o = default_opts();
        let rep = fetch("not a url", &o);
        assert_eq!(rep.ok, false);
        assert_eq!(rep.status, 0);
        assert_eq!(rep.attempts, 1);
        assert!(rep.error.starts_with("InvalidUrl"));
    }

    #[test]
    fn failed_reply_shape() {
        let r = failed_reply("https://x/", 3, "Dns");
        assert_eq!(r.status, 0);
        assert_eq!(r.ok, false);
        assert_eq!(r.bytes, 0);
        assert_eq!(r.truncated, false);
        assert_eq!(r.attempts, 3);
    }
}
