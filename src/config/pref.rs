//! 偏好模型 + 校验 + 默认值 + 友好错误（阶段 B）.
//!
//! schema（config/feeds.toml）：
//!   [default]  engine site keywords blacklist weight max_age_hours enabled
//!   [[feeds]]  name url engine site keywords weight max_age_hours blacklist enabled note
//!
//! 规则：unknown key = warning（ignored），bad type / missing url / out-of-range = error（fatal）.
//! 友好别名：link=url, title/id=name, keyword/query=keywords, max_age=max_age_hours,
//!           block/blocked=blacklist, desc/description=note.

use crate::config::toml::Doc;
use crate::config::toml::Table;
use crate::config::toml::Value;
use crate::config::toml::render_block;

#[derive(Clone)]
pub struct Feed {
    pub name: String,
    pub url: String,
    pub engine: String,
    pub site: String,
    pub keywords: Vec<String>,
    pub blacklist: Vec<String>,
    pub weight: f64,
    pub max_age_hours: i64,
    pub enabled: bool,
    pub note: String,
    pub line: usize,
}

#[derive(Clone)]
pub struct Defaults {
    pub engine: String,
    pub site: String,
    pub keywords: Vec<String>,
    pub blacklist: Vec<String>,
    pub weight: f64,
    pub max_age_hours: i64,
    pub enabled: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum IssueKind {
    Error,
    Warning,
}

#[derive(Clone)]
pub struct Issue {
    pub line: usize,
    pub kind: IssueKind,
    pub message: String,
}

pub struct Prefs {
    pub defaults: Defaults,
    pub feeds: Vec<Feed>,
    pub issues: Vec<Issue>,
}

pub fn kind_text(k: IssueKind) -> String {
    if k == IssueKind::Error {
        return "error".to_string();
    }
    return "warning".to_string();
}

/// engine 白名单（决定 D3）：bing / baidu / auto.
fn is_valid_engine(v: &str) -> bool {
    return v == "bing" || v == "baidu" || v == "auto";
}

fn known_feed_key(k: &str) -> bool {
    return k == "name" || k == "url" || k == "engine" || k == "site" || k == "keywords" ||
        k == "blacklist" || k == "weight" || k == "max_age_hours" || k == "enabled" || k == "note";
}

fn known_default_key(k: &str) -> bool {
    return k == "engine" || k == "site" || k == "keywords" || k == "blacklist" || k == "weight" ||
        k == "max_age_hours" || k == "enabled";
}

/// 友好别名：max_age == max_age_hours, link == url, keyword == keywords ...
fn canonical_key(k: &str) -> String {
    let t = k.to_lowercase();
    if t == "link" { return "url".to_string(); }
    if t == "title" || t == "id" { return "name".to_string(); }
    if t == "keyword" || t == "query" { return "keywords".to_string(); }
    if t == "max_age" { return "max_age_hours".to_string(); }
    if t == "block" || t == "blocked" { return "blacklist".to_string(); }
    if t == "desc" || t == "description" { return "note".to_string(); }
    return t.to_string();
}

/// 1.0 -> "1.0"（TOML 里 round-trip 作为 float，display 稳定）.
pub fn fmt_num(v: f64) -> String {
    let s = format!("{}", v);
    if !s.contains(".") && !s.contains("e") && !s.contains("E") {
        return format!("{}.0", v);
    }
    return s;
}

pub fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => { return Some(s.to_string()); }
        Value::Int(i) => { return Some(format!("{}", i)); }
        Value::Flt(f) => { return Some(fmt_num(*f)); }
        Value::Bool(b) => { return Some(if *b { "true".to_string() } else { "false".to_string() }); }
        Value::Arr(_) => { return None; }
    }
}

pub fn as_double(v: &Value) -> Option<f64> {
    match v {
        Value::Flt(f) => { return Some(*f); }
        Value::Int(i) => { return Some(*i as f64); }
        Value::Str(s) => {
            match s.trim().parse::<f64>() { Ok(x) => { return Some(x); } Err(_) => { return None; } }
        }
        Value::Bool(_) => { return None; }
        Value::Arr(_) => { return None; }
    }
}

pub fn as_long(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => { return Some(*i); }
        Value::Flt(f) => { return Some(*f as i64); }
        Value::Str(s) => {
            match s.trim().parse::<i64>() { Ok(x) => { return Some(x); } Err(_) => { return None; } }
        }
        Value::Bool(_) => { return None; }
        Value::Arr(_) => { return None; }
    }
}

pub fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => { return Some(*b); }
        Value::Str(s) => {
            let t = s.trim().to_lowercase();
            if t == "true" { return Some(true); }
            if t == "false" { return Some(false); }
            return None;
        }
        _ => { return None; }
    }
}

pub fn split_comma_list(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in s.split(",") {
        let t = part.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    return out;
}

pub fn as_string_list(v: &Value) -> Vec<String> {
    match v {
        Value::Arr(items) => {
            let mut out: Vec<String> = Vec::new();
            for it in items {
                match as_string(it) {
                    Some(s) => {
                        let t = s.trim();
                        if !t.is_empty() {
                            out.push(t.to_string());
                        }
                    }
                    None => {}
                }
            }
            return out;
        }
        Value::Str(s) => { return split_comma_list(s); }
        _ => { return Vec::new(); }
    }
}

/// [default] table -> Defaults（unknown key = warning，bad type = warning）.
fn apply_defaults(d: Defaults, keys: &[(String, Value)]) -> (Defaults, Vec<Issue>) {
    let mut out: Defaults = d;
    let mut issues: Vec<Issue> = Vec::new();
    for (raw_key, v) in keys {
        let ck = canonical_key(raw_key);
        if !known_default_key(&ck) {
            issues.push(Issue { line: 0, kind: IssueKind::Warning, message: format!("unknown key \"{}\" in [default] ignored", raw_key) });
            continue;
        }
        if ck == "engine" {
            match as_string(v) {
                Some(s) => { out.engine = s.trim().to_lowercase(); }
                None => { issues.push(Issue { line: 0, kind: IssueKind::Warning, message: "engine must be a string: bing | baidu | auto".to_string() }); }
            }
        } else if ck == "site" {
            match as_string(v) {
                Some(s) => { out.site = s.trim().to_string(); }
                None => { issues.push(Issue { line: 0, kind: IssueKind::Warning, message: "site must be a string (host, e.g. bing.com)".to_string() }); }
            }
        } else if ck == "keywords" {
            out.keywords = as_string_list(v);
        } else if ck == "blacklist" {
            out.blacklist = as_string_list(v);
        } else if ck == "weight" {
            match as_double(v) {
                Some(x) => { out.weight = x; }
                None => { issues.push(Issue { line: 0, kind: IssueKind::Warning, message: "weight must be a number, e.g. weight = 2.5".to_string() }); }
            }
        } else if ck == "max_age_hours" {
            match as_long(v) {
                Some(x) => { out.max_age_hours = x; }
                None => { issues.push(Issue { line: 0, kind: IssueKind::Warning, message: "max_age_hours must be an integer, e.g. 48".to_string() }); }
            }
        } else if ck == "enabled" {
            match as_bool(v) {
                Some(x) => { out.enabled = x; }
                None => { issues.push(Issue { line: 0, kind: IssueKind::Warning, message: "enabled must be true or false".to_string() }); }
            }
        }
    }
    return (out, issues);
}

/// [[feeds]] table -> Feed（missing/unknown/bad value -> Issue）.
fn feed_from_table(t: &Table, d: Defaults) -> (Feed, Vec<Issue>) {
    let mut issues: Vec<Issue> = Vec::new();
    let mut f = Feed {
        name: "".to_string(),
        url: "".to_string(),
        engine: d.engine,
        site: d.site,
        keywords: d.keywords,
        blacklist: d.blacklist,
        weight: d.weight,
        max_age_hours: d.max_age_hours,
        enabled: d.enabled,
        note: "".to_string(),
        line: t.line,
    };
    for (raw_key, v) in &t.keys {
        let ck = canonical_key(raw_key);
        if !known_feed_key(&ck) {
            issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: format!("unknown key \"{}\" in [[feeds]] ignored (known: name,url,engine,site,keywords,blacklist,weight,max_age_hours,enabled,note)", raw_key) });
            continue;
        }
        if ck == "url" {
            match as_string(v) {
                Some(s) => { f.url = s.trim().to_string(); }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Error, message: "url must be a quoted string: url = \"https://...\"".to_string() }); }
            }
        } else if ck == "name" {
            match as_string(v) {
                Some(s) => { f.name = s.trim().to_string(); }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "name must be a string".to_string() }); }
            }
        } else if ck == "engine" {
            match as_string(v) {
                Some(s) => { f.engine = s.trim().to_lowercase(); }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Error, message: "engine must be a string: bing | baidu | auto".to_string() }); }
            }
        } else if ck == "site" {
            match as_string(v) {
                Some(s) => { f.site = s.trim().to_string(); }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "site must be a string (host)".to_string() }); }
            }
        } else if ck == "keywords" {
            f.keywords = as_string_list(v);
        } else if ck == "blacklist" {
            f.blacklist = as_string_list(v);
        } else if ck == "weight" {
            match as_double(v) {
                Some(x) => { f.weight = x; }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Error, message: "weight must be a number in (0, 10]".to_string() }); }
            }
        } else if ck == "max_age_hours" {
            match as_long(v) {
                Some(x) => { f.max_age_hours = x; }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Error, message: "max_age_hours must be an integer >= 1".to_string() }); }
            }
        } else if ck == "enabled" {
            match as_bool(v) {
                Some(x) => { f.enabled = x; }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "enabled must be true or false".to_string() }); }
            }
        } else if ck == "note" {
            match as_string(v) {
                Some(s) => { f.note = s.trim().to_string(); }
                None => { issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "note must be a string".to_string() }); }
            }
        }
    }
    if f.name.is_empty() {
        f.name = derive_name(&f.url);
    }
    return (f, issues);
}

pub fn derive_name(url: &str) -> String {
    let mut s: &str = url;
    match s.find("//") {
        Some(i) => { s = &s[i + 2..]; }
        None => {}
    }
    let bytes = s.as_bytes();
    let mut cut = bytes.len();
    for i in 0..bytes.len() {
        let b = bytes[i];
        if b == 0x2F || b == 0x3F || b == 0x26 || b == 0x20 {
            cut = i;
            break;
        }
    }
    let host: &str = &s[..cut];
    if host.starts_with("www.") {
        return host[4..].to_string();
    }
    if host.is_empty() {
        return "feed".to_string();
    }
    return host.to_string();
}

/// 校验一个 feed：返回 issues（kind = error 是 fatal）.
pub fn check_feed(f: &Feed) -> Vec<Issue> {
    let mut out: Vec<Issue> = Vec::new();
    if f.url.is_empty() {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: "[[feeds]] missing url: add url = \"https://...\"".to_string() });
    } else if !f.url.starts_with("http://") && !f.url.starts_with("https://") {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("url must start with http:// or https:// (got: \"{}\")", f.url) });
    }
    if !is_valid_engine(&f.engine) {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("engine \"{}\" invalid: expected bing | baidu | auto", f.engine) });
    }
    if f.weight <= 0.0 || f.weight > 10.0 {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("weight {} out of range (0, 10]", fmt_num(f.weight)) });
    }
    if f.max_age_hours < 1 || f.max_age_hours > 1000000 {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("max_age_hours {} out of range 1..1000000", f.max_age_hours) });
    }
    if f.keywords.len() > 32 {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("too many keywords ({} > 32)", f.keywords.len()) });
    }
    for k in &f.keywords {
        if k.is_empty() {
            out.push(Issue { line: f.line, kind: IssueKind::Error, message: "empty keyword in keywords list".to_string() });
        }
        if k.chars().count() > 128 {
            out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("keyword too long ({} chars > 128): {}", k.chars().count(), k) });
        }
    }
    if f.blacklist.len() > 64 {
        out.push(Issue { line: f.line, kind: IssueKind::Error, message: format!("too many blacklist patterns ({} > 64)", f.blacklist.len()) });
    }
    if f.keywords.is_empty() {
        out.push(Issue { line: f.line, kind: IssueKind::Warning, message: "no keywords: this feed is fetched as-is (no search query generated)".to_string() });
    }
    return out;
}

/// Doc -> Prefs：[default] + [[feeds]]，dedup by url，unknown table = warning.
pub fn from_doc(doc: &Doc) -> Prefs {
    let mut d: Defaults = builtin_defaults();
    let mut issues: Vec<Issue> = Vec::new();
    for t in &doc.tables {
        if t.name == "default" || t.name == "defaults" {
            if t.array {
                issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "[[default]] ignored: use [default] for defaults".to_string() });
                continue;
            }
            let applied = apply_defaults(d, &t.keys);
            d = applied.0;
            for i in applied.1 {
                issues.push(i);
            }
        } else if t.name == "" {
            for e in &t.keys {
                issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: format!("top-level key \"{}\" ignored (put it under [default] or [[feeds]])", e.0) });
            }
        } else if t.name != "feeds" {
            issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: format!("unknown table \"{}\" ignored (expected [default] or [[feeds]])", t.name) });
        }
    }
    let mut feeds: Vec<Feed> = Vec::new();
    for t in &doc.tables {
        if t.name != "feeds" {
            continue;
        }
        if !t.array {
            issues.push(Issue { line: t.line, kind: IssueKind::Warning, message: "[feeds] ignored: use [[feeds]] (array of tables)".to_string() });
            continue;
        }
        let made = feed_from_table(t, d.clone());
        let f = made.0;
        for i in made.1 {
            issues.push(i);
        }
        for i in check_feed(&f) {
            issues.push(i);
        }
        let mut dup = false;
        for g in &feeds {
            if g.url == f.url && !f.url.is_empty() {
                dup = true;
            }
        }
        if dup {
            issues.push(Issue { line: f.line, kind: IssueKind::Warning, message: format!("duplicate feed url (ignored for dedup): {}", f.url) });
            continue;
        }
        feeds.push(f);
    }
    return Prefs { defaults: d, feeds: feeds, issues: issues };
}

pub fn builtin_defaults() -> Defaults {
    return Defaults {
        engine: "auto".to_string(),
        site: "".to_string(),
        keywords: Vec::new(),
        blacklist: Vec::new(),
        weight: 1.0,
        max_age_hours: 48,
        enabled: true,
    };
}

/// string list → our Value.Arr（writer helper）.
fn to_str_array(v: &[String]) -> Value {
    let mut items = Vec::new();
    for s in v {
        items.push(Value::Str(s.to_string()));
    }
    return Value::Arr(items);
}

/// render one [[feeds]] block：field selection here, TOML text via the crate serializer.
pub fn render_feed(f: &Feed) -> String {
    let mut keys = Vec::new();
    if !f.name.is_empty() {
        keys.push(("name".to_string(), Value::Str(f.name.to_string())));
    }
    keys.push(("url".to_string(), Value::Str(f.url.to_string())));
    keys.push(("engine".to_string(), Value::Str(f.engine.to_string())));
    if !f.site.is_empty() {
        keys.push(("site".to_string(), Value::Str(f.site.to_string())));
    }
    if !f.keywords.is_empty() {
        keys.push(("keywords".to_string(), to_str_array(&f.keywords)));
    }
    if !f.blacklist.is_empty() {
        keys.push(("blacklist".to_string(), to_str_array(&f.blacklist)));
    }
    keys.push(("weight".to_string(), Value::Flt(f.weight)));
    keys.push(("max_age_hours".to_string(), Value::Int(f.max_age_hours)));
    if !f.enabled {
        keys.push(("enabled".to_string(), Value::Bool(false)));
    }
    if !f.note.is_empty() {
        keys.push(("note".to_string(), Value::Str(f.note.to_string())));
    }
    return render_block("feeds", true, keys);
}

pub fn count_errors(issues: &[Issue]) -> usize {
    let mut n: usize = 0;
    for i in issues {
        if i.kind == IssueKind::Error {
            n += 1;
        }
    }
    return n;
}

pub fn count_warnings(issues: &[Issue]) -> usize {
    let mut n: usize = 0;
    for i in issues {
        if i.kind == IssueKind::Warning {
            n += 1;
        }
    }
    return n;
}

#[cfg(test)]
mod tests {
    use crate::config::toml::parse;
    use crate::config::pref::builtin_defaults;
    use crate::config::pref::canonical_key;
    use crate::config::pref::from_doc;
    use crate::config::pref::count_errors;
    use crate::config::pref::render_feed;

    #[test]
    fn defaults_are_sane() {
        let d = builtin_defaults();
        assert_eq!(d.engine, "auto");
        assert_eq!(d.max_age_hours, 48);
    }

    #[test]
    fn alias_keys_are_canonical() {
        assert_eq!(canonical_key("max_age"), "max_age_hours");
        assert_eq!(canonical_key("LINK"), "url");
    }

    #[test]
    fn bad_feed_reports_error() {
        let text = "[[feeds]]\nurl = \"ftp://x\"\nengine = \"duck\"\n";
        let doc = parse(&text).unwrap();
        let prefs = from_doc(&doc);
        assert_eq!(count_errors(&prefs.issues), 2);
    }

    #[test]
    fn render_round_trips() {
        let text = "[[feeds]]\nurl = \"https://a\"\nkeywords = [\"x\"]\nweight = 2\n";
        let doc = parse(&text).unwrap();
        let prefs = from_doc(&doc);
        assert_eq!(prefs.feeds.len(), 1);
        let block = render_feed(&prefs.feeds[0]);
        let again = parse(&block).unwrap();
        let prefs2 = from_doc(&again);
        assert_eq!(prefs2.feeds.len(), 1);
        assert_eq!(prefs2.feeds[0].url, "https://a");
    }

    #[test]
    fn dedup_by_url() {
        let text = "[[feeds]]\nurl = \"https://a\"\n[[feeds]]\nurl = \"https://a\"\n";
        let doc = parse(&text).unwrap();
        let prefs = from_doc(&doc);
        assert_eq!(prefs.feeds.len(), 1);
    }

    #[test]
    fn alias_feed_keys_are_accepted() {
        let text = "[[feeds]]\nlink = \"https://b.com/feed\"\nkeyword = \"rust, cargo\"\nmax_age = 12\n";
        let doc = parse(&text).unwrap();
        let prefs = from_doc(&doc);
        assert_eq!(prefs.feeds.len(), 1);
        assert_eq!(prefs.feeds[0].url, "https://b.com/feed");
        assert_eq!(prefs.feeds[0].max_age_hours, 12);
        assert_eq!(prefs.feeds[0].keywords.len(), 2);
    }
}
