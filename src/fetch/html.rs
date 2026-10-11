//! C3: HTML 正文抽取 — DOM-lite 容错解析 + link/text density 启发式.
//!
//! flow:
//!   1. tolerant arena parse（畸形 HTML 不 panic；script/style/noscript/template/svg/iframe 内容整段丢弃）
//!   2. 每个元素累计子树统计：text 字数 / 元素数 / <a> 链接字数（reverse-index 自底向上）
//!   3. 评分：text_density = text/(tags+1)，link_density = link_text/text
//!        score = text_density * (1 - link_density)^2 * classWeight
//!      （容器级：div/article/section/main/td/body…；nav/footer/sidebar/comment/ads 直接排除）
//!   4. 渲染选中容器的文本：块级标签插入换行 → 逐行 collapse/trim → 去空行
//!
//! extract() 返回 title + 正文 + 指标；article_from_html() 提升为 C2 的统一 Article.
//!
//! rustc 1.97 traps honored: explicit return, Vec::new(), &str slices, .to_string().

use crate::fetch::feed::collapse_ws;
use crate::fetch::feed::site_of;
use crate::fetch::feed::Article;

/// 少量 named entities（够用即可；数字实体全支持）。
const NAMED: [(&str, &str); 24] = [
    ("amp", "&"), ("lt", "<"), ("gt", ">"), ("quot", "\""), ("apos", "'"),
    ("nbsp", " "), ("ensp", "\u{2002}"), ("emsp", "\u{2003}"), ("thinsp", "\u{2009}"),
    ("mdash", "\u{2014}"), ("ndash", "\u{2013}"), ("hellip", "\u{2026}"),
    ("laquo", "\u{00AB}"), ("raquo", "\u{00BB}"), ("middot", "\u{00B7}"),
    ("copy", "\u{00A9}"), ("reg", "\u{00AE}"), ("trade", "\u{2122}"),
    ("times", "\u{00D7}"), ("divide", "\u{00F7}"), ("yen", "\u{00A5}"),
    ("bull", "\u{2022}"), ("sect", "\u{00A7}"), ("deg", "\u{00B0}"),
];

fn entity_value(name: &str) -> Option<String> {
    if name.starts_with('#') {
        let body: &str = &name[1..];
        let hex = body.starts_with('x') || body.starts_with('X');
        let digits: &str = if hex { &body[1..] } else { body };
        let code: Result<u32, _> = if hex {
            u32::from_str_radix(digits, 16)
        } else {
            digits.parse::<u32>()
        };
        if let Ok(cp) = code {
            if let Some(c) = char::from_u32(cp) {
                return Some(c.to_string());
            }
        }
        return None;
    }
    let lower = name.to_lowercase();
    for (k, v) in NAMED {
        if k == lower {
            return Some(v.to_string());
        }
    }
    return None;
}

/// 解码 HTML entities（named subset + &#NN; + &#xHH;）；未知实体原样保留。
pub fn decode_entities(s: &str) -> String {
    let chs: Vec<char> = s.chars().collect();
    let n = chs.len();
    let mut out = String::new();
    let mut i: usize = 0;
    while i < n {
        if chs[i] == '&' {
            let mut j: usize = i + 1;
            let mut limit: usize = j + 12;
            if limit > n { limit = n; }
            let mut semi: usize = 0;
            while j < limit {
                if chs[j] == ';' { semi = j; break; }
                if chs[j].is_whitespace() { break; }
                j += 1;
            }
            if semi > i + 1 {
                let name: String = chs[i + 1..semi].iter().collect();
                if let Some(rep) = entity_value(&name) {
                    out.push_str(&rep);
                    i = semi + 1;
                    continue;
                }
            }
            out.push('&');
            i += 1;
            continue;
        }
        out.push(chs[i]);
        i += 1;
    }
    return out;
}

/// one arena node: element or "#text".
#[derive(Clone)]
pub struct Node {
    pub tag: String,
    pub id: String,
    pub class: String,
    pub attrs: Vec<(String, String)>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub text: String,
}

fn empty_node(tag: &str, parent: Option<usize>) -> Node {
    return Node {
        tag: tag.to_string(),
        id: "".to_string(),
        class: "".to_string(),
        attrs: Vec::new(),
        parent: parent,
        children: Vec::new(),
        text: "".to_string(),
    };
}

/// DOM-lite：节点 0 = "#root"。
pub struct Dom {
    pub nodes: Vec<Node>,
}

fn is_void(tag: &str) -> bool {
    return tag == "area" || tag == "base" || tag == "br" || tag == "col" || tag == "embed"
        || tag == "hr" || tag == "img" || tag == "input" || tag == "link" || tag == "meta"
        || tag == "param" || tag == "source" || tag == "track" || tag == "wbr"
        || tag == "command" || tag == "keygen";
}

/// 内容整段跳过的原始文本元素。
fn is_raw(tag: &str) -> bool {
    return tag == "script" || tag == "style" || tag == "noscript" || tag == "template"
        || tag == "svg" || tag == "iframe";
}

fn is_block(tag: &str) -> bool {
    return tag == "p" || tag == "div" || tag == "section" || tag == "article" || tag == "main"
        || tag == "header" || tag == "footer" || tag == "aside" || tag == "nav" || tag == "ul"
        || tag == "ol" || tag == "li" || tag == "table" || tag == "tr" || tag == "td"
        || tag == "th" || tag == "thead" || tag == "tbody" || tag == "blockquote" || tag == "pre"
        || tag == "h1" || tag == "h2" || tag == "h3" || tag == "h4" || tag == "h5" || tag == "h6"
        || tag == "figure" || tag == "figcaption" || tag == "hr" || tag == "br" || tag == "dl"
        || tag == "dt" || tag == "dd" || tag == "form" || tag == "address" || tag == "caption";
}

/// 仅这些容器级标签可以被选中为正文块（避免选中单个 <p>）。
fn is_container(tag: &str) -> bool {
    return tag == "div" || tag == "article" || tag == "section" || tag == "main"
        || tag == "td" || tag == "blockquote" || tag == "body" || tag == "ul" || tag == "ol"
        || tag == "table" || tag == "figure" || tag == "dd" || tag == "form";
}

const NEGATIVE: [&str; 24] = [
    "nav", "menu", "footer", "sidebar", "comment", "share", "social", "related", "recomm",
    "advert", "promo", "breadcrumb", "pagination", "pager", "copyright", "subscribe",
    "newsletter", "login", "signup", "cookie", "banner", "widget", "toolbar", "sponsor",
];

const POSITIVE: [&str; 12] = [
    "article", "content", "post", "entry", "story", "main", "body", "text", "detail",
    "read", "markdown", "prose",
];

fn has_needle(node: &Node, needles: &[&str]) -> bool {
    let mut hay = node.class.to_lowercase();
    if !node.id.is_empty() {
        hay = hay + " " + &node.id.to_lowercase();
    }
    if hay.is_empty() {
        return false;
    }
    for nd in needles {
        if hay.contains(nd) {
            return true;
        }
    }
    return false;
}

fn read_name(chs: &[char], start: usize) -> (String, usize) {
    let n = chs.len();
    let mut j: usize = start;
    let mut name = String::new();
    while j < n {
        let c = chs[j];
        if c.is_whitespace() || c == '>' || c == '/' {
            break;
        }
        name.push(c);
        j += 1;
    }
    return (name, j);
}

/// 从 '<' 处解析 start tag → (tag, attrs, after '>', self_close)。
fn read_start_tag(chs: &[char], i: usize) -> (String, Vec<(String, String)>, usize, bool) {
    let n = chs.len();
    let (name, mut j) = read_name(chs, i + 1);
    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut self_close = false;
    loop {
        while j < n && chs[j].is_whitespace() { j += 1; }
        if j >= n { break; }
        let c = chs[j];
        if c == '>' { j += 1; break; }
        if c == '/' {
            if j + 1 < n && chs[j + 1] == '>' { self_close = true; j += 2; break; }
            j += 1;
            continue;
        }
        let mut an = String::new();
        while j < n {
            let c2 = chs[j];
            if c2.is_whitespace() || c2 == '=' || c2 == '>' || c2 == '/' { break; }
            an.push(c2);
            j += 1;
        }
        if an.is_empty() {
            j += 1;
            continue;
        }
        while j < n && chs[j].is_whitespace() { j += 1; }
        let mut av = String::new();
        if j < n && chs[j] == '=' {
            j += 1;
            while j < n && chs[j].is_whitespace() { j += 1; }
            if j < n && (chs[j] == '"' || chs[j] == '\'') {
                let q = chs[j];
                j += 1;
                while j < n && chs[j] != q { av.push(chs[j]); j += 1; }
                if j < n { j += 1; }
            } else {
                while j < n {
                    let c2 = chs[j];
                    if c2.is_whitespace() || c2 == '>' { break; }
                    av.push(c2);
                    j += 1;
                }
            }
        }
        attrs.push((an.to_lowercase(), decode_entities(&av)));
    }
    return (name, attrs, j, self_close);
}

/// 跳过原始文本元素内容，返回 close tag 之后的位置。
fn skip_raw(chs: &[char], from: usize, tag: &str) -> usize {
    let n = chs.len();
    let needle: String = format!("</{}", tag);
    let nl: Vec<char> = needle.chars().collect();
    let mut j: usize = from;
    while j < n {
        if chs[j] == '<' {
            let mut ok = j + nl.len() <= n;
            if ok {
                let mut k: usize = 0;
                while k < nl.len() {
                    if chs[j + k].to_ascii_lowercase() != nl[k] { ok = false; break; }
                    k += 1;
                }
            }
            if ok {
                let mut m: usize = j + nl.len();
                while m < n && chs[m] != '>' { m += 1; }
                if m < n { return m + 1; }
                return n;
            }
        }
        j += 1;
    }
    return n;
}

impl Dom {
    /// 容错解析：畸形标签只影响局部结构，绝不 panic。
    pub fn parse(html: &str) -> Dom {
        let chs: Vec<char> = html.chars().collect();
        let n = chs.len();
        let mut nodes: Vec<Node> = Vec::new();
        nodes.push(empty_node("#root", None));
        let mut stack: Vec<usize> = Vec::new();
        stack.push(0);
        let mut i: usize = 0;
        while i < n {
            if chs[i] != '<' {
                let start: usize = i;
                while i < n && chs[i] != '<' { i += 1; }
                let raw: String = chs[start..i].iter().collect();
                let decoded = decode_entities(&raw);
                let p: usize = *stack.last().unwrap();
                if decoded.trim().is_empty() {
                    let mut dup = false;
                    if let Some(&last) = nodes[p].children.last() {
                        if nodes[last].tag == "#text" && nodes[last].text.trim().is_empty() { dup = true; }
                    }
                    if !dup && decoded.len() > 0 {
                        let mut tn = empty_node("#text", Some(p));
                        tn.text = " ".to_string();
                        let idx = nodes.len();
                        nodes.push(tn);
                        nodes[p].children.push(idx);
                    }
                } else {
                    let mut tn = empty_node("#text", Some(p));
                    tn.text = decoded;
                    let idx = nodes.len();
                    nodes.push(tn);
                    nodes[p].children.push(idx);
                }
                continue;
            }
            if i + 3 < n && chs[i + 1] == '!' && chs[i + 2] == '-' && chs[i + 3] == '-' {
                let mut j: usize = i + 4;
                while j + 2 < n {
                    if chs[j] == '-' && chs[j + 1] == '-' && chs[j + 2] == '>' { j += 3; break; }
                    j += 1;
                }
                if j + 2 >= n { j = n; }
                i = j;
                continue;
            }
            if i + 1 < n && (chs[i + 1] == '!' || chs[i + 1] == '?') {
                let mut j: usize = i + 1;
                while j < n && chs[j] != '>' { j += 1; }
                if j < n { j += 1; }
                i = j;
                continue;
            }
            if i + 1 < n && chs[i + 1] == '/' {
                let (name, after) = read_name(&chs, i + 2);
                let lname = name.to_lowercase();
                let mut k: usize = stack.len();
                while k > 1 {
                    k -= 1;
                    if nodes[stack[k]].tag == lname {
                        stack.truncate(k);
                        break;
                    }
                }
                // read_name 停在 '>' 之前 → 必须把 close tag 整体吃掉
                let mut after2: usize = after;
                while after2 < n && chs[after2] != '>' { after2 += 1; }
                if after2 < n { after2 += 1; }
                i = after2;
                continue;
            }
            let (name, attrs, after, self_close) = read_start_tag(&chs, i);
            let lname = name.to_lowercase();
            if lname.is_empty() {
                i = i + 1;
                continue;
            }
            let p: usize = *stack.last().unwrap();
            let mut node = empty_node(&lname, Some(p));
            for a in &attrs {
                if a.0 == "id" { node.id = a.1.clone(); }
                if a.0 == "class" { node.class = a.1.clone(); }
                node.attrs.push((a.0.clone(), a.1.clone()));
            }
            let idx = nodes.len();
            nodes.push(node);
            nodes[p].children.push(idx);
            i = after;
            if is_void(&lname) || self_close {
                continue;
            }
            if is_raw(&lname) {
                i = skip_raw(&chs, after, &lname);
                continue;
            }
            stack.push(idx);
        }
        return Dom { nodes: nodes };
    }

    pub fn len(&self) -> usize {
        return self.nodes.len();
    }

    pub fn tag(&self, i: usize) -> &str {
        return &self.nodes[i].tag;
    }

    pub fn class(&self, i: usize) -> &str {
        return &self.nodes[i].class;
    }

    pub fn attr(&self, i: usize, name: &str) -> Option<String> {
        for a in &self.nodes[i].attrs {
            if a.0 == name {
                return Some(a.1.clone());
            }
        }
        return None;
    }

    /// 元素（+ 后代）的文本，whitespace collapse。
    pub fn text(&self, i: usize) -> String {
        let mut out = String::new();
        self.collect_text(i, &mut out);
        return collapse_ws(&out);
    }

    fn collect_text(&self, i: usize, out: &mut String) {
        if self.nodes[i].tag == "#text" {
            out.push_str(&self.nodes[i].text);
            return;
        }
        for c in &self.nodes[i].children {
            self.collect_text(*c, out);
        }
    }

    /// 后代元素（document order，不含自身）。
    pub fn descendants(&self, i: usize) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        self.walk_desc(i, &mut out);
        return out;
    }

    fn walk_desc(&self, i: usize, out: &mut Vec<usize>) {
        for c in &self.nodes[i].children {
            if self.nodes[*c].tag != "#text" {
                out.push(*c);
            }
            self.walk_desc(*c, out);
        }
    }

    pub fn first_descendant(&self, i: usize, tag: &str) -> Option<usize> {
        for c in &self.nodes[i].children {
            if self.nodes[*c].tag == tag {
                return Some(*c);
            }
            let deeper = self.first_descendant(*c, tag);
            if deeper.is_some() {
                return deeper;
            }
        }
        return None;
    }

    /// class 含 needle 的所有元素。
    pub fn by_class(&self, needle: &str) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for i in 0..self.nodes.len() {
            if self.nodes[i].tag == "#text" { continue; }
            if self.nodes[i].class.to_lowercase().contains(needle) { out.push(i); }
        }
        return out;
    }

    pub fn by_tag(&self, tag: &str) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for i in 0..self.nodes.len() {
            if self.nodes[i].tag == tag { out.push(i); }
        }
        return out;
    }
}

/// 抽取结果 + 可读指标。
pub struct Extracted {
    pub title: String,
    pub text: String,
    pub text_len: usize,
    pub link_density: f64,
    pub tag: String,
    pub id: String,
    pub class: String,
}

fn non_ws_len(s: &str) -> usize {
    let mut n: usize = 0;
    for c in s.chars() {
        if !c.is_whitespace() { n += 1; }
    }
    return n;
}

/// 页面标题：<title> → og:title → <h1>。
pub fn title_of(dom: &Dom) -> String {
    for i in dom.by_tag("title") {
        let t = dom.text(i);
        if !t.is_empty() { return t; }
    }
    for i in 0..dom.len() {
        if dom.tag(i) == "meta" {
            let prop = dom.attr(i, "property").unwrap_or_default();
            let name = dom.attr(i, "name").unwrap_or_default();
            if prop == "og:title" || name == "og:title" || name == "twitter:title" {
                if let Some(c) = dom.attr(i, "content") {
                    let t = collapse_ws(&c);
                    if !t.is_empty() { return t; }
                }
            }
        }
    }
    for i in dom.by_tag("h1") {
        let t = dom.text(i);
        if !t.is_empty() { return t; }
    }
    return "".to_string();
}

fn render_block_text(dom: &Dom, i: usize, out: &mut String) {
    let node = &dom.nodes[i];
    if node.tag == "#text" {
        out.push_str(&node.text);
        return;
    }
    let block = is_block(&node.tag);
    if block && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for c in &node.children {
        if dom.nodes[*c].tag != "#text" && has_needle(&dom.nodes[*c], &NEGATIVE) {
            continue;
        }
        render_block_text(dom, *c, out);
    }
    if block {
        out.push('\n');
    }
}

fn normalize_lines(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.split('\n') {
        let t = collapse_ws(line);
        if t.is_empty() { continue; }
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&t);
    }
    return out;
}

/// 主入口：抽 title + 正文文本 + 指标。
pub fn extract(html: &str) -> Extracted {
    let dom = Dom::parse(html);
    let total_n = dom.len();
    let mut text_chars: Vec<usize> = vec![0; total_n];
    let mut tag_count: Vec<usize> = vec![0; total_n];
    let mut link_chars: Vec<usize> = vec![0; total_n];
    for i in 0..total_n {
        let t = &dom.nodes[i].tag;
        if t == "#text" {
            text_chars[i] = non_ws_len(&dom.nodes[i].text);
            tag_count[i] = 0;
        } else {
            text_chars[i] = 0;
            tag_count[i] = 1;
        }
    }
    let mut i: usize = total_n;
    while i > 1 {
        i -= 1;
        let p: usize = match dom.nodes[i].parent { Some(p) => p, None => 0 };
        text_chars[p] += text_chars[i];
        tag_count[p] += tag_count[i];
        let contribute: usize = if dom.nodes[i].tag == "a" { text_chars[i] } else { link_chars[i] };
        link_chars[p] += contribute;
    }
    let mut bad_ancestor: Vec<bool> = vec![false; total_n];
    for i in 1..total_n {
        let mut bad = false;
        let mut cur: usize = i;
        while cur != 0 {
            if has_needle(&dom.nodes[cur], &NEGATIVE) { bad = true; break; }
            cur = match dom.nodes[cur].parent { Some(p) => p, None => 0 };
        }
        bad_ancestor[i] = bad;
    }
    let mut best: usize = 0;
    let mut best_score: f64 = 0.0;
    let mut fallback: usize = 0;
    let mut fallback_text: usize = 0;
    for i in 1..total_n {
        let tag = &dom.nodes[i].tag;
        if tag == "#text" { continue; }
        if tag_count[i] > 0 && text_chars[i] > fallback_text && !bad_ancestor[i] {
            fallback_text = text_chars[i];
            fallback = i;
        }
        if !is_container(tag) { continue; }
        if tag_count[i] < 1 { continue; }
        if text_chars[i] < 120 { continue; }
        if bad_ancestor[i] { continue; }
        let ld: f64 = link_chars[i] as f64 / text_chars[i] as f64;
        if ld > 0.6 { continue; }
        let density: f64 = text_chars[i] as f64 / (tag_count[i] as f64 + 1.0);
        let mut w: f64 = 1.0;
        if has_needle(&dom.nodes[i], &POSITIVE) { w = w * 1.4; }
        if tag == "body" || tag == "ul" || tag == "ol" || tag == "table" { w = w * 0.8; }
        let score: f64 = density * (1.0 - ld) * (1.0 - ld) * w;
        if score > best_score {
            best_score = score;
            best = i;
        }
    }
    if best == 0 { best = fallback; }
    let mut raw = String::new();
    render_block_text(&dom, best, &mut raw);
    let text = normalize_lines(&raw);
    let ld_final: f64 = if text_chars[best] > 0 {
        link_chars[best] as f64 / text_chars[best] as f64
    } else {
        0.0
    };
    let title = title_of(&dom);
    return Extracted {
        title: title,
        text_len: non_ws_len(&text),
        text: text,
        link_density: ld_final,
        tag: dom.nodes[best].tag.clone(),
        id: dom.nodes[best].id.clone(),
        class: dom.nodes[best].class.clone(),
    };
}

/// 页面语言：<html lang="..">。
pub fn lang_of(html: &str) -> String {
    let dom = Dom::parse(html);
    for i in dom.by_tag("html") {
        if let Some(l) = dom.attr(i, "lang") {
            let t = collapse_ws(&l);
            if !t.is_empty() { return t; }
        }
    }
    return "".to_string();
}

/// HTML 页面 → C2 的统一 Article（body = 抽取正文，site = host）。
pub fn article_from_html(html: &str, url: &str) -> Article {
    let ex = extract(html);
    let mut title = ex.title;
    if title.is_empty() { title = site_of(url); }
    return Article {
        title: title,
        url: url.to_string(),
        body: ex.text,
        published: "".to_string(),
        site: site_of(url),
        lang: lang_of(html),
    };
}

#[cfg(test)]
mod tests {
    use super::article_from_html;
    use super::decode_entities;
    use super::extract;
    use super::Dom;

    fn page() -> String {
        return "<!-- comment --><html lang=\"zh-CN\"><head><title>示例文章 — 站点</title>\
<style>body { color: red }</style><script>var tracker = 1;</script></head>\
<body><nav class=\"site-nav\"><a href=\"/a\">首页</a><a href=\"/b\">关于</a></nav>\
<main><article class=\"post-content\"><h1>示例文章标题</h1>\
<p>这是正文的第一段，包含足够多的字符来让 text density 评分选中它，并且链接很少。</p>\
<p>第二段继续展开论述 &amp; 细节，&lt;code&gt; 已被实体解码，中文逗号、顿号也有。</p>\
<pre>fn main() { println!(\"hi\"); }</pre></article></main>\
<aside class=\"sidebar\"><a href=\"/x\">相关阅读一</a><a href=\"/y\">相关阅读二</a></aside>\
<footer class=\"copyright\">版权所有 2026 — 不要出现在正文里</footer></body></html>".to_string();
    }

    #[test]
    fn extracts_article_body() {
        let html = page();
        let ex = extract(&html);
        assert_eq!(ex.title, "示例文章 — 站点");
        assert!(ex.text.contains("这是正文的第一段"));
        assert!(ex.text.contains("第二段继续展开论述"));
        assert!(ex.text.contains("fn main()"));
        assert_eq!(ex.text.contains("不要出现在正文里"), false);
        assert_eq!(ex.text.contains("var tracker"), false);
        assert_eq!(ex.text.contains("color: red"), false);
        assert_eq!(ex.text.contains("相关阅读"), false);
    }

    #[test]
    fn html_lang_is_captured() {
        let html = page();
        let art = article_from_html(&html, "https://example.com/post/1");
        assert_eq!(art.lang, "zh-CN");
        assert_eq!(art.site, "example.com");
        assert_eq!(art.url, "https://example.com/post/1");
        assert!(art.body.contains("这是正文的第一段"));
    }

    #[test]
    fn entities_decode() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#65; &#x4e2d; &hellip;"), "a & b <c> A 中 …");
        assert_eq!(decode_entities("100% &unknown; ok"), "100% &unknown; ok");
    }

    #[test]
    fn malformed_html_does_not_panic() {
        let html = "<div><p>hello <b>world</div></p><a href=x>tail<tr><td>cell";
        let ex = extract(html);
        assert!(ex.text.contains("hello"));
    }

    #[test]
    fn dom_parses_class_and_attrs() {
        let dom = Dom::parse("<div id=\"main\" class=\"a b\" data-x='1'><p>hi</p></div>");
        let divs = dom.by_class("a b");
        assert_eq!(divs.len(), 1);
        assert_eq!(dom.attr(divs[0], "data-x").unwrap(), "1");
        assert_eq!(dom.text(divs[0]), "hi");
    }

    #[test]
    fn nav_only_page_still_returns_text() {
        let html = "<body><nav><a href=\"/\">one</a><a href=\"/2\">two</a></nav></body>";
        let ex = extract(html);
        assert!(ex.text_len < 100);
    }
}
