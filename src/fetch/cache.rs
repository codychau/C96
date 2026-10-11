//! C1: HTTP cache — ETag / Last-Modified / content-type / body per URL, stored as TOML
//!   via the crate layer（阶段 B 的 flat Doc model）.
//! load() tolerates a missing/broken file（empty cache, never fatal）; save() is atomic
//!   （path.tmp → rename）and rewrites the whole file（machine-owned, no user comments）.
//! fold() implements the conditional-GET merge: a 304 keeps the cached validators/body,
//!   a fresh 2xx replaces them（a stale ETag must never survive a 200）.

use crate::config::toml::parse;
use crate::config::toml::render_block;
use crate::config::toml::Value;
use crate::fetch::http::Reply;

#[derive(Clone)]
pub struct Entry {
    pub url: String,
    pub etag: String,
    pub last_modified: String,
    pub content_type: String,
    pub status: usize,
    pub body: String,
}

fn str_of(v: &Value) -> String {
    match v {
        Value::Str(s) => { return s.to_string(); }
        _ => { return "".to_string(); }
    }
}

fn int_of(v: &Value) -> usize {
    match v {
        Value::Int(i) => { if *i < 0 { return 0; } return *i as usize; }
        _ => { return 0; }
    }
}

/// read the cache file; missing or unparsable → empty cache（never fatal）.
pub fn load(path: &str) -> Vec<Entry> {
    let text = match std::fs::read_to_string(path) {
        Err(_) => { return Vec::new(); }
        Ok(s) => { s }
    };
    let doc = match parse(&text) {
        Err(_) => { return Vec::new(); }
        Ok(d) => { d }
    };
    let mut out = Vec::new();
    for t in doc.tables {
        if t.name != "entry" { continue; }
        let mut e = Entry {
            url: "".to_string(),
            etag: "".to_string(),
            last_modified: "".to_string(),
            content_type: "".to_string(),
            status: 0,
            body: "".to_string(),
        };
        for pair in t.keys {
            let (k, v) = pair;
            if k == "url" { e.url = str_of(&v); }
            if k == "etag" { e.etag = str_of(&v); }
            if k == "last_modified" { e.last_modified = str_of(&v); }
            if k == "content_type" { e.content_type = str_of(&v); }
            if k == "body" { e.body = str_of(&v); }
            if k == "status" { e.status = int_of(&v); }
        }
        if e.url.len() > 0 { out.push(e); }
    }
    return out;
}

pub fn find(entries: &Vec<Entry>, url: &str) -> Option<Entry> {
    for e in entries {
        if e.url == url { return Some(e.clone()); }
    }
    return None;
}

pub fn drop(entries: &Vec<Entry>, url: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for e in entries {
        if e.url != url { out.push(e.clone()); }
    }
    return out;
}

/// keep the newest max_entries（append order = newest last）; 0 = no cap.
pub fn prune(entries: Vec<Entry>, max_entries: usize) -> Vec<Entry> {
    if max_entries == 0 { return entries; }
    if entries.len() <= max_entries { return entries; }
    let mut out = Vec::new();
    let mut i: usize = entries.len() - max_entries;
    while i < entries.len() {
        out.push(entries[i].clone());
        i += 1;
    }
    return out;
}

/// fold one Reply into the cache for url: drop the old entry, append the new one（newest last）.
/// 304：body 永远是 cached body（304 无 body）；validators 若被服务器回显则采用回显值，
///   否则保留 cached 值；无 cached entry 的 304 → 不写入.
/// 2xx：全新 validators + body（a stale ETag must never survive a 200）.
/// failure（!ok）：保留旧 entry.
pub fn fold(entries: &Vec<Entry>, url: &str, rep: &Reply, max_entries: usize) -> Vec<Entry> {
    if !rep.ok { return prune(entries.clone(), max_entries); }  // failure keeps the old entry
    let mut out = drop(entries, url);
    if rep.not_modified {
        let cached = find(entries, url);
        if cached.is_none() { return prune(out, max_entries); }
        let c: &Entry = cached.as_ref().unwrap();
        let merged = if rep.etag.len() > 0 {
            Entry {
                url: url.to_string(), etag: rep.etag.to_string(), last_modified: rep.last_modified.to_string(),
                content_type: rep.content_type.to_string(), status: rep.status, body: c.body.to_string(),
            }
        } else {
            Entry {
                url: url.to_string(), etag: c.etag.to_string(), last_modified: c.last_modified.to_string(),
                content_type: c.content_type.to_string(), status: rep.status, body: c.body.to_string(),
            }
        };
        out.push(merged);
        return prune(out, max_entries);
    }
    out.push(Entry {
        url: url.to_string(),
        etag: rep.etag.to_string(),
        last_modified: rep.last_modified.to_string(),
        content_type: rep.content_type.to_string(),
        status: rep.status,
        body: rep.body.to_string(),
    });
    return prune(out, max_entries);
}

/// create a directory chain（this std only has single-level create_dir）.
pub fn ensure_dir(dir: &str) -> bool {
    if dir.len() == 0 { return true; }
    let bytes = dir.as_bytes();
    let n = bytes.len();
    let mut i: usize = 1;
    while i < n {
        if bytes[i] == 0x5C || bytes[i] == 0x2F {
            let part: &str = &dir[..i];
            if part.len() > 0 {
                match std::fs::create_dir(part) {
                    Err(_) => { }
                    Ok(_) => { }
                }
            }
        }
        i += 1;
    }
    match std::fs::create_dir(&dir) {
        Err(_) => { return dir_exists(&dir); }
        Ok(_) => { return true; }
    }
}

fn dir_exists(path: &str) -> bool {
    match std::fs::metadata(&path) {
        Err(_) => { return false; }
        Ok(_) => { return true; }
    }
}

/// parent dir of a path（"" when the file sits in the cwd）.
pub fn dir_of(path: &str) -> String {
    let bytes = path.as_bytes();
    let n = bytes.len();
    let mut idx: usize = 0;
    let mut i: usize = 0;
    while i < n {
        if bytes[i] == 0x5C || bytes[i] == 0x2F { idx = i; }
        i += 1;
    }
    if idx == 0 { return "".to_string(); }
    return path[..idx].to_string();
}

/// rewrite the whole cache file（this file is machine-owned, no user comments）.
pub fn save(path: &str, entries: &Vec<Entry>) -> bool {
    let mut merged = "# C96 HTTP cache — ETag / Last-Modified / body, regenerated by fetch\n\n".to_string();
    for e in entries {
        let mut keys = Vec::new();
        keys.push(("url".to_string(), Value::Str(e.url.to_string())));
        keys.push(("etag".to_string(), Value::Str(e.etag.to_string())));
        keys.push(("last_modified".to_string(), Value::Str(e.last_modified.to_string())));
        keys.push(("content_type".to_string(), Value::Str(e.content_type.to_string())));
        keys.push(("status".to_string(), Value::Int(e.status as i64)));
        keys.push(("body".to_string(), Value::Str(e.body.to_string())));
        merged = merged + &render_block("entry", true, keys);
        merged += "\n";
    }
    let dir = dir_of(path);
    if dir.len() > 0 { if !ensure_dir(&dir) { return false; } }
    let tmp = path.to_owned() + ".tmp";
    match std::fs::write(&tmp, merged.as_bytes()) {
        Err(_) => { return false; }
        Ok(_) => { }
    }
    match std::fs::rename(&tmp, path) {
        Err(_) => { return false; }
        Ok(_) => { return true; }
    }
}

#[cfg(test)]
mod tests {
    use crate::fetch::cache::Entry;
    use crate::fetch::cache::load;
    use crate::fetch::cache::save;
    use crate::fetch::cache::find;
    use crate::fetch::cache::drop;
    use crate::fetch::cache::fold;
    use crate::fetch::cache::prune;
    use crate::fetch::cache::dir_of;
    use crate::fetch::cache::ensure_dir;
    use crate::fetch::http::Reply;

    fn mk_entry(url: &str, etag: &str, body: &str) -> Entry {
        return Entry {
            url: url.to_string(),
            etag: etag.to_string(),
            last_modified: "".to_string(),
            content_type: "text/html".to_string(),
            status: 200,
            body: body.to_string(),
        };
    }

    #[test]
    fn save_load_round_trip() {
        let path = "_scratch/c1_cache_round_trip.toml";
        let mut es = Vec::new();
        es.push(mk_entry("https://a/", "W/\"v1\"", "line1\nline2 # not a comment \"quoted\""));
        es.push(mk_entry("https://b/", "", "second"));
        assert!(save(&path, &es));
        let back = load(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].body, es[0].body);
        assert_eq!(back[0].etag, "W/\"v1\"");
        assert_eq!(back[1].url, "https://b/");
    }

    #[test]
    fn broken_cache_is_empty() {
        let path = "_scratch/c1_cache_broken.toml";
        match std::fs::write(&path, "[[entry]\nthis is not toml\n") {
            Err(_) => { }
            Ok(_) => { }
        }
        let back = load(&path);
        assert_eq!(back.len(), 0);
    }

    #[test]
    fn find_and_drop() {
        let mut es = Vec::new();
        es.push(mk_entry("https://a/", "e1", "a"));
        es.push(mk_entry("https://b/", "e2", "b"));
        let hit = find(&es, "https://b/");
        assert!(hit.is_some());
        assert_eq!(hit.as_ref().unwrap().body, "b");
        assert!(find(&es, "https://zz/").is_none());
        assert_eq!(drop(&es, "https://a/").len(), 1);
    }

    #[test]
    fn fold_304_keeps_validators_and_body() {
        let mut es = Vec::new();
        es.push(mk_entry("https://m/", "W/\"v1\"", "cached body"));
        let rep = Reply {
            url: "https://m/".to_string(),
            final_url: "https://m/".to_string(),
            status: 304,
            ok: true,
            not_modified: true,
            content_type: "text/plain".to_string(),
            etag: "".to_string(),
            last_modified: "".to_string(),
            body: "".to_string(),
            bytes: 0,
            truncated: false,
            attempts: 1,
            error: "".to_string(),
        };
        let next = fold(&es, "https://m/", &rep, 0);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].body, "cached body");
        assert_eq!(next[0].etag, "W/\"v1\"");
        assert_eq!(next[0].content_type, "text/html");
    }

    #[test]
    fn fold_200_replaces_validators() {
        let mut es = Vec::new();
        es.push(mk_entry("https://m/", "W/\"stale\"", "old"));
        let rep = Reply {
            url: "https://m/".to_string(),
            final_url: "https://m/".to_string(),
            status: 200,
            ok: true,
            not_modified: false,
            content_type: "application/rss+xml".to_string(),
            etag: "".to_string(),
            last_modified: "Mon, 06 Oct 2026 03:00:00 GMT".to_string(),
            body: "fresh".to_string(),
            bytes: 5,
            truncated: false,
            attempts: 1,
            error: "".to_string(),
        };
        let next = fold(&es, "https://m/", &rep, 0);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].etag, "");
        assert_eq!(next[0].body, "fresh");
        assert_eq!(next[0].content_type, "application/rss+xml");
    }

    #[test]
    fn fold_failure_keeps_old_entry() {
        let mut es = Vec::new();
        es.push(mk_entry("https://m/", "W/\"v1\"", "kept"));
        let rep = Reply {
            url: "https://m/".to_string(), final_url: "".to_string(), status: 0, ok: false,
            not_modified: false, content_type: "".to_string(), etag: "".to_string(),
            last_modified: "".to_string(), body: "".to_string(), bytes: 0, truncated: false,
            attempts: 3, error: "Dns".to_string(),
        };
        let next = fold(&es, "https://m/", &rep, 0);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].body, "kept");
    }

    #[test]
    fn prune_keeps_newest() {
        let mut es = Vec::new();
        es.push(mk_entry("https://1/", "", "1"));
        es.push(mk_entry("https://2/", "", "2"));
        es.push(mk_entry("https://3/", "", "3"));
        let p = prune(es.clone(), 2);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].url, "https://2/");
        assert_eq!(prune(es.clone(), 0).len(), 3);
        assert_eq!(prune(es, 9).len(), 3);
    }

    #[test]
    fn dir_helpers() {
        assert_eq!(dir_of("cache/http_cache.toml"), "cache");
        assert_eq!(dir_of("solo.toml"), "");
        assert_eq!(ensure_dir("cache"), true);
    }

    fn reply_304(etag: &str, ct: &str) -> Reply {
        return Reply {
            url: "https://m/".to_string(), final_url: "https://m/".to_string(),
            status: 304, ok: true, not_modified: true,
            content_type: ct.to_string(), etag: etag.to_string(), last_modified: "".to_string(),
            body: "".to_string(), bytes: 0, truncated: false, attempts: 1, error: "".to_string(),
        };
    }

    #[test]
    fn fold_304_echoed_etag_keeps_cached_body() {
        let mut es = Vec::new();
        es.push(mk_entry("https://m/", "W/\"v1\"", "cached body"));
        let rep = reply_304("W/\"v2\"", "text/html");
        let next = fold(&es, "https://m/", &rep, 0);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].etag, "W/\"v2\"");
        assert_eq!(next[0].body, "cached body");
    }

    #[test]
    fn fold_304_without_cache_writes_nothing() {
        let es = Vec::new();
        let rep = reply_304("", "");
        let next = fold(&es, "https://m/", &rep, 0);
        assert_eq!(next.len(), 0);
    }

    #[test]
    fn content_type_and_status_round_trip() {
        let path = "_scratch/c1_cache_ct.toml";
        let mut e = mk_entry("https://feed/", "W/\"x\"", "<rss/>");
        e.content_type = "application/rss+xml".to_string();
        e.status = 200;
        let mut es = Vec::new();
        es.push(e);
        assert!(save(&path, &es));
        let back = load(&path);
        assert_eq!(back[0].content_type, "application/rss+xml");
        assert_eq!(back[0].status, 200);
    }
}
