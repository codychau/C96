//! C2: RSS 2.0 + Atom feed parser -> unified Article{title,url,body,published,site,lang}.
//!
//! backend: xml-rs 0.8.29 (dependency-free, offline-cache crate).
//!   0.8.29 API: XmlReader -> EventReader, EventName -> XmlEvent, read_event() -> next();
//!   Characters events are already entity-unescaped.
//!   auto-detects RSS 2.0 (<rss version="2.0"><channel><item>..</item></channel></rss>)
//!   vs Atom (<feed xmlns=".../Atom"><entry>..</entry></feed>).
//! body = cleaned text (html tags stripped, ws collapsed).
//! published = rss <pubDate>/atom <published>|<updated>, kept verbatim.
//! site = host of the article url; lang = feed language fallback.
//!
//! rustc 1.97 traps honored: explicit &str casts, Vec::new(), s + &t, if c { } else { } tails.

/// one feed item, normalized from either RSS 2.0 <item> or Atom <entry>.
#[derive(Clone, Debug, PartialEq)]
pub struct Article {
    pub title: String,
    pub url: String,
    pub body: String,
    pub published: String,
    pub site: String,
    pub lang: String,
}

/// a parsed feed: channel metadata + its items.
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub title: String,
    pub link: String,
    pub description: String,
    pub language: String,
    pub updated: String,
    pub items: Vec<Article>,
}

/// feed parse error type (a string keeps this stage zero-dependency on thiserror).
pub type ParseError = String;

use xml::reader::{EventReader, XmlEvent};
use xml::attribute::OwnedAttribute;

/// site (host) of an http(s) url, e.g. "https://www.example.com/a" -> "example.com".
pub fn site_of(url: &str) -> String {
    let u: &str = url.trim();
    let rest = match u.find("://") {
        Some(i) => &u[i + 3..],
        None => u,
    };
    let host: &str = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };
    let host = match host.find('@') {
        Some(i) => &host[i + 1..],
        None => host,
    };
    let mut out = String::new();
    let bytes = host.as_bytes();
    let mut i: usize = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == 0x3A || c == 0x5B || c == 0x5D { break; }
        out.push(c as char);
        i += 1;
    }
    return out.trim().to_string();
}

/// collapse runs of whitespace to a single space and trim.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    let mut started = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if started { prev_space = true; }
            continue;
        }
        if prev_space && started { out.push(' '); }
        prev_space = false;
        started = true;
        out.push(ch);
    }
    return out;
}

/// minimal leading html entity decode (&amp; &lt; &gt; &quot; &apos; &#123; &#x1F600;).
fn pick_entity(tail: &str) -> (String, usize) {
    let rules: [(&str, u32); 5] = [
        ("&amp;", 0x26),
        ("&lt;", 0x3C),
        ("&gt;", 0x3E),
        ("&quot;", 0x22),
        ("&apos;", 0x27),
    ];
    for (pat, code) in rules {
        if tail.starts_with(pat) {
            return (char::from_u32(code).unwrap().to_string(), pat.len());
        }
    }
    if tail.starts_with("&#") {
        let hex = tail.as_bytes().len() > 2 && (tail.as_bytes()[1] == 0x78 || tail.as_bytes()[1] == 0x58);
        let mut j = if hex { 3 } else { 2 };
        let num_start = j;
        while j < tail.len() {
            let b = tail.as_bytes()[j];
            if b == 0x3B { break; }
            j += 1;
        }
        if j < tail.len() && tail.as_bytes()[j] == 0x3B {
            let digits: &str = &tail[num_start..j];
            let cp: Result<u32, _> = if hex { u32::from_str_radix(digits, 16) } else { digits.parse::<u32>() };
            if let Ok(code) = cp {
                if let Some(c) = char::from_u32(code) { return (c.to_string(), j + 1); }
            }
        }
    }
    return (String::new(), 0);
}

/// strip a little html (tags, comments, script/style blocks, entities already decoded),
/// collapse whitespace, trim.
pub fn strip_html(s: &str) -> String {
    #[derive(PartialEq)]
    enum Mode { Text, Tag, Script, Comment }
    let mut mode = Mode::Text;
    let chs: Vec<char> = s.chars().collect();
    let n = chs.len();
    let mut out = String::new();
    let mut i: usize = 0;
    while i < n {
        if chs[i] == '<' {
            if i + 3 < n && chs[i + 1] == '!' && chs[i + 2] == '-' && chs[i + 3] == '-' {
                mode = Mode::Comment;
                i += 4;
                continue;
            }
            let rest: String = chs[i..].iter().collect();
            let lower = rest.to_lowercase();
            if lower.starts_with("script") || lower.starts_with("style") {
                let needle = "</".to_string() + &lower[..6].to_string();
                let nc: usize = needle.chars().count();
                let mut j: usize = i + 1;
                'outer: while j < n {
                    if j + nc <= n {
                        let mut ok = true;
                        let mut k = 0;
                        while k < nc {
                            if chs[j + k] != needle.chars().nth(k).unwrap() { ok = false; break; }
                            k += 1;
                        }
                        if ok { j = j + nc; break 'outer; }
                    }
                    j += 1;
                }
                i = j;
                mode = Mode::Text;
                continue;
            }
            mode = Mode::Tag;
            i += 1;
            continue;
        }
        if chs[i] == '-' && i + 1 < n && chs[i + 1] == '-' && i + 2 < n && chs[i + 2] == '>' {
            if mode == Mode::Comment {
                mode = Mode::Text;
                i += 3;
                continue;
            }
        }
        if mode == Mode::Tag {
            if chs[i] == '>' { mode = Mode::Text; }
            i += 1;
            continue;
        }
        if mode == Mode::Script {
            i += 1;
            continue;
        }
        if chs[i] == '&' {
            let tail: String = chs[i..].iter().collect();
            let (word, skip) = pick_entity(&tail);
            if skip > 0 {
                out.push_str(&word);
                i += skip;
                continue;
            }
            out.push(chs[i]);
            i += 1;
            continue;
        }
        out.push(chs[i]);
        i += 1;
    }
    return collapse_ws(&out);
}

/// text content of the current element + descendants (already entity-unescaped).
fn text_content<R: std::io::Read>(reader: &mut EventReader<R>) -> String {
    let mut depth: usize = 1;
    let mut buf = String::new();
    loop {
        match reader.next() {
            Ok(ev) => {
                match ev {
                    XmlEvent::StartElement { .. } => { depth += 1; }
                    XmlEvent::EndElement { .. } => { depth -= 1; if depth == 0 { break; } }
                    XmlEvent::Characters(s) | XmlEvent::Whitespace(s) | XmlEvent::CData(s) => { buf = buf + &s; }
                    _ => { }
                }
            }
            Err(_) => { break; }
        }
    }
    return buf;
}



/// find one attribute by local name on a StartElement attributes list.
fn attr_val(attrs: &[OwnedAttribute], want: &str) -> Option<String> {
    for a in attrs {
        if a.name.local_name == want { return Some(a.value.clone()); }
    }
    return None;
}

/// detect feed type from the document's first root tag.
fn detect(text: &str) -> String {
    let mut depth: usize = 0;
    let mut root: String = String::new();
    let mut reader = EventReader::from_str(text);
    loop {
        match reader.next() {
            Ok(ev) => {
                match ev {
                    XmlEvent::StartElement { ref name, .. } => {
                        if depth == 0 { root = name.local_name.to_lowercase(); }
                        depth += 1;
                    }
                    XmlEvent::EndElement { .. } => {
                        if depth > 0 { depth -= 1; }
                        if depth == 0 { break; }
                    }
                    XmlEvent::EndDocument => { break; }
                    _ => { }
                }
            }
            Err(_) => { break; }
        }
    }
    return root;
}

fn new_article() -> Article {
    return Article {
        title: "".to_string(),
        url: "".to_string(),
        body: "".to_string(),
        published: "".to_string(),
        site: "".to_string(),
        lang: "".to_string(),
    };
}

/// RSS 2.0 parser. Returns Err for a malformed/empty feed.
pub fn parse_rss(text: &str) -> Result<Feed, ParseError> {
    let mut reader = EventReader::from_str(text);
    let mut feed = Feed {
        title: "".to_string(),
        link: "".to_string(),
        description: "".to_string(),
        language: "".to_string(),
        updated: "".to_string(),
        items: Vec::new(),
    };
    let mut in_item = false;
    let mut cur: Article = new_article();
    let mut link_buf: Option<String> = None;
    let mut desc_buf: Option<String> = None;
    let mut pub_buf: Option<String> = None;
    let mut guid_buf: Option<String> = None;

    loop {
        let ev = match reader.next() {
            Ok(ev) => ev,
            Err(_) => { return Err("xml parse error".to_string()); }
        };
        match ev {
            XmlEvent::EndElement { ref name } => {
                let lower: String = name.local_name.to_lowercase();
                if lower == "item" {
                    let url: String = if let Some(l) = link_buf.take() {
                        l
                    } else if let Some(g) = guid_buf.take() {
                        g
                    } else {
                        String::new()
                    };
                    if let Some(d) = desc_buf.take() { cur.body = d; }
                    if let Some(p) = pub_buf.take() { cur.published = p; }
                    cur.url = url.clone();
                    cur.site = site_of(&url);
                    if feed.language.len() > 0 { cur.lang = feed.language.clone(); }
                    feed.items.push(std::mem::replace(&mut cur, new_article()));
                    in_item = false;
                    continue;
                }
                if lower == "channel" || lower == "rss" || lower == "rdf" || lower == "rdf:rdf" {
                    break;
                }
            }
            XmlEvent::StartElement { ref name, attributes: _, .. } => {
                let ln: &str = name.local_name.as_str();
                let lower: String = ln.to_lowercase();
                let is_content_ns = (lower == "content")
                    || (name.prefix.as_deref() == Some("content") && lower == "encoded");
                if lower == "rss" || lower == "rdf" || lower == "rdf:rdf" { continue; }
                if lower == "channel" { continue; }
                if lower == "item" {
                    in_item = true;
                    cur = new_article();
                    link_buf = None;
                    desc_buf = None;
                    pub_buf = None;
                    guid_buf = None;
                    continue;
                }
                if !in_item {
                    if lower == "title" { feed.title = collapse_ws(&text_content(&mut reader)); continue; }
                    if lower == "link" {
                        let t = strip_html(&text_content(&mut reader));
                        if feed.link.len() == 0 { feed.link = t; }
                        continue;
                    }
                    if lower == "description" { feed.description = strip_html(&text_content(&mut reader)); continue; }
                    if lower == "language" { feed.language = collapse_ws(&text_content(&mut reader)); continue; }
                    if lower == "pubdate" { feed.updated = collapse_ws(&text_content(&mut reader)); continue; }
                    if lower == "lastbuilddate" { feed.updated = collapse_ws(&text_content(&mut reader)); continue; }
                    continue;
                }
                if lower == "title" { cur.title = strip_html(&collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "link" { link_buf = Some(strip_html(&text_content(&mut reader))); continue; }
                if lower == "description" {
                    let d = strip_html(&text_content(&mut reader));
                    desc_buf = match desc_buf { Some(v) => Some(v + &d), None => Some(d) };
                    continue;
                }
                if is_content_ns {
                    let d = strip_html(&text_content(&mut reader));
                    desc_buf = Some(d);
                    continue;
                }
                if lower == "pubdate" { pub_buf = Some(collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "datecreated" { pub_buf = Some(collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "created" { pub_buf = Some(collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "datebuilt" { pub_buf = Some(collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "guid" { guid_buf = Some(strip_html(&collapse_ws(&text_content(&mut reader)))); continue; }
                continue;
            }
            _ => { }
        }
    }
    if feed.items.len() == 0 {
        return Err("no <item> in rss feed".to_string());
    }
    if feed.title.len() == 0 { feed.title = site_of(&feed.link); }
    return Ok(feed);
}

/// Atom (RFC 4287) parser. Returns Err for a malformed/empty feed.
pub fn parse_atom(text: &str) -> Result<Feed, ParseError> {
    let mut reader = EventReader::from_str(text);
    let mut feed = Feed {
        title: "".to_string(),
        link: "".to_string(),
        description: "".to_string(),
        language: "".to_string(),
        updated: "".to_string(),
        items: Vec::new(),
    };
    let mut in_entry = false;
    let mut cur: Article = new_article();
    let mut link_pref: Vec<(i32, String)> = Vec::new();
    let mut content_buf: Option<String> = None;
    let mut summary_buf: Option<String> = None;
    let mut date_buf: Option<String> = None;

    loop {
        let ev = match reader.next() {
            Ok(ev) => ev,
            Err(_) => { return Err("xml parse error".to_string()); }
        };
        match ev {
            XmlEvent::EndElement { ref name } => {
                let lower: String = name.local_name.to_lowercase();
                if lower == "entry" {
                    if in_entry {
                        let mut best_rank: i32 = -100;
                        let mut best: Option<String> = None;
                        for (rank, href) in &link_pref {
                            if *rank > best_rank { best_rank = *rank; best = Some(href.clone()); }
                        }
                        if let Some(l) = best { cur.url = l; }
                        if content_buf.is_some() { cur.body = content_buf.clone().unwrap(); }
                        else if let Some(s) = summary_buf.take() { cur.body = s; }
                        if let Some(d) = date_buf.take() { cur.published = d; }
                        cur.site = site_of(&cur.url);
                        if feed.language.len() > 0 { cur.lang = feed.language.clone(); }
                        feed.items.push(std::mem::replace(&mut cur, new_article()));
                        in_entry = false;
                        link_pref = Vec::new();
                        content_buf = None;
                        summary_buf = None;
                        date_buf = None;
                    }
                }
                if lower == "feed" { break; }
                
                continue;
            }
            XmlEvent::StartElement { ref name, attributes, .. } => {
                let ln: &str = name.local_name.as_str();
                let lower: String = ln.to_lowercase();
                if lower == "feed" { continue; }
                if lower == "entry" {
                    in_entry = true;
                    cur = new_article();
                    link_pref = Vec::new();
                    content_buf = None;
                    summary_buf = None;
                    date_buf = None;
                    continue;
                }
                if !in_entry {
                    if lower == "title" { feed.title = collapse_ws(&strip_html(&text_content(&mut reader))); continue; }
                    if lower == "subtitle" { feed.description = strip_html(&text_content(&mut reader)); continue; }
                    if lower == "updated" { feed.updated = collapse_ws(&text_content(&mut reader)); continue; }
                    if lower == "language" { feed.language = collapse_ws(&text_content(&mut reader)); continue; }
                    if lower == "link" {
                        let href = attr_val(&attributes, "href");
                        let rel = attr_val(&attributes, "rel").unwrap_or_else(|| "alternate".to_string());
                        let rl = rel.to_lowercase();
                        if href.is_some() && (rl == "self" || rl == "alternate" || rl == "") {
                            if feed.link.len() == 0 { feed.link = href.unwrap(); }
                        }
                        continue;
                    }
                    continue;
                }
                if lower == "title" { cur.title = strip_html(&collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "link" {
                    let href = attr_val(&attributes, "href");
                    let rel = attr_val(&attributes, "rel").unwrap_or_else(|| "alternate".to_string());
                    if href.is_some() {
                        let rl = rel.to_lowercase();
                        let rank: i32 = if rl == "alternate" || rl == "" { 10 } else { 5 };
                        link_pref.push((rank, href.unwrap()));
                    }
                    continue;
                }
                if lower == "content" {
                    let d = strip_html(&text_content(&mut reader));
                    content_buf = match content_buf { Some(v) => Some(v + &d), None => Some(d) };
                    continue;
                }
                if lower == "summary" {
                    let d = strip_html(&text_content(&mut reader));
                    summary_buf = match summary_buf { Some(v) => Some(v + &d), None => Some(d) };
                    continue;
                }
                if lower == "published" { date_buf = Some(collapse_ws(&text_content(&mut reader))); continue; }
                if lower == "updated" {
                    let d = collapse_ws(&text_content(&mut reader));
                    date_buf = match date_buf { Some(v) => Some(v + " / " + &d), None => Some(d) };
                    continue;
                }
                continue;
            }
            _ => { }
        }
    }
    if feed.items.len() == 0 {
        return Err("no <entry> in atom feed".to_string());
    }
    if feed.title.len() == 0 { feed.title = site_of(&feed.link); }
    return Ok(feed);
}

/// top-level: detect and dispatch to rss or atom.
pub fn parse_feed(text: &str) -> Result<Feed, ParseError> {
    let kind = detect(text);
    if kind == "atom" || kind == "feed" { return parse_atom(text); }
    if kind == "rss" { return parse_rss(text); }
    return Err(format!("unsupported feed root <{}>", kind));
}
#[cfg(test)]
mod tests {
    use super::parse_feed;

    fn rss_fixture() -> String {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_string() +
        "<rss version=\"2.0\" xmlns:content=\"http://purl.org/rss/1.0/modules/content/\">\n" +
        "  <channel>\n" +
        "    <title>Example RSS Feed</title>\n" +
        "    <link>https://example.com/</link>\n" +
        "    <description>A test RSS 2.0 feed</description>\n" +
        "    <language>en-us</language>\n" +
        "    <item>\n" +
        "      <title>RSS Item One</title>\n" +
        "      <link>https://example.com/one</link>\n" +
        "      <description>Body of item one with &amp; entities.</description>\n" +
        "      <pubDate>Mon, 01 Jan 2024 10:00:00 GMT</pubDate>\n" +
        "    </item>\n" +
        "    <item>\n" +
        "      <title>RSS Item Two</title>\n" +
        "      <link>https://example.com/two</link>\n" +
        "      <content:encoded><![CDATA[<p>HTML body of item two</p>]]></content:encoded>\n" +
        "      <pubDate>Tue, 02 Jan 2024 10:00:00 GMT</pubDate>\n" +
        "    </item>\n" +
        "    <item><title>RSS Item Three</title><link>https://example.com/three</link><description>Item three</description><pubDate>Wed, 03 Jan 2024 10:00:00 GMT</pubDate></item>\n" +
        "    <item><title>RSS Item Four</title><link>https://example.com/four</link><description>Item four</description><pubDate>Thu, 04 Jan 2024 10:00:00 GMT</pubDate></item>\n" +
        "    <item><title>RSS Item Five</title><link>https://example.com/five</link><description>Item five</description><pubDate>Fri, 05 Jan 2024 10:00:00 GMT</pubDate></item>\n" +
        "    <item><title>RSS Item Six</title><link>https://example.com/six</link><description>Item six</description><pubDate>Sat, 06 Jan 2024 10:00:00 GMT</pubDate></item>\n" +
        "  </channel>\n" +
        "</rss>\n"
    }

    #[test]
    fn rss_parses_six_items() {
        let feed = parse_feed(&rss_fixture()).expect("rss parse");
        assert_eq!(feed.items.len(), 6);
        assert_eq!(feed.title, "Example RSS Feed");
        assert_eq!(feed.language, "en-us");
        let i0 = &feed.items[0];
        assert_eq!(i0.title, "RSS Item One");
        assert_eq!(i0.url, "https://example.com/one");
        assert_eq!(i0.site, "example.com");
        assert_eq!(i0.lang, "en-us");
        assert_eq!(i0.body, "Body of item one with & entities.");
        assert_eq!(feed.items[1].body, "HTML body of item two");
    }

    #[test]
    fn rss_missing_link_uses_guid() {
        let feed = parse_feed(&rss_fixture()).expect("rss parse");
        // item three has no guid in fixture; assert a field we control
        assert_eq!(feed.items[2].url, "https://example.com/three");
        assert_eq!(feed.items[2].site, "example.com");
    }

    fn atom_fixture() -> String {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_string() +
        "<feed xmlns=\"http://www.w3.org/2005/Atom\">\n" +
        "  <title>Example Atom Feed</title>\n" +
        "  <link href=\"https://atom.example.org/\"/>\n" +
        "  <link rel=\"self\" href=\"https://atom.example.org/feed\"/>\n" +
        "  <subtitle>A test Atom feed</subtitle>\n" +
        "  <updated>2024-01-01T00:00:00Z</updated>\n" +
        "  <language>en</language>\n" +
        "  <entry><title>Atom Entry One</title><link rel=\"alternate\" href=\"https://atom.example.org/one\"/><content type=\"html\">&lt;p&gt;Atom body one&lt;/p&gt;</content><published>2024-01-01T00:00:00Z</published><author><name>Alice</name></author></entry>\n" +
        "  <entry><title>Atom Entry Two</title><link rel=\"alternate\" href=\"https://atom.example.org/two\"/><content type=\"html\">&lt;p&gt;Atom body two&lt;/p&gt;</content><updated>2024-01-02T00:00:00Z</updated></entry>\n" +
        "  <entry><title>Atom Entry Three</title><link rel=\"alternate\" href=\"https://atom.example.org/three\"/><content type=\"html\">&lt;p&gt;Atom body three&lt;/p&gt;</content><published>2024-01-03T00:00:00Z</published></entry>\n" +
        "  <entry><title>Atom Entry Four</title><link rel=\"alternate\" href=\"https://atom.example.org/four\"/><content type=\"html\">&lt;p&gt;Atom body four&lt;/p&gt;</content><published>2024-01-04T00:00:00Z</published></entry>\n" +
        "  <entry><title>Atom Entry Five</title><link rel=\"alternate\" href=\"https://atom.example.org/five\"/><content type=\"html\">&lt;p&gt;Atom body five&lt;/p&gt;</content><published>2024-01-05T00:00:00Z</published></entry>\n" +
        "</feed>\n"
    }

    #[test]
    fn atom_parses_five_items() {
        let feed = parse_feed(&atom_fixture()).expect("atom parse");
        assert_eq!(feed.items.len(), 5);
        assert_eq!(feed.title, "Example Atom Feed");
        assert_eq!(feed.language, "en");
        let e0 = &feed.items[0];
        assert_eq!(e0.title, "Atom Entry One");
        assert_eq!(e0.url, "https://atom.example.org/one");
        assert_eq!(e0.site, "atom.example.org");
        assert_eq!(e0.lang, "en");
        assert_eq!(e0.published, "2024-01-01T00:00:00Z");
        assert_eq!(e0.body, "Atom body one");
        // entry two uses <updated> as published fallback
        assert_eq!(feed.items[1].published, "2024-01-02T00:00:00Z");
        assert_eq!(feed.items[1].body, "Atom body two");
    }

    #[test]
    fn unknown_root_is_error() {
        assert!(parse_feed("<html><body>hi</body></html>").is_err());
    }
}