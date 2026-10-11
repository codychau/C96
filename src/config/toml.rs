//! TOML 层（阶段 B 修订 2026-10-07）：official `toml` crate v1.1.6 (spec-1.1.0).
//!
//! 手写 parser/writer 已删除 → full TOML 1.1 syntax accepted:
//!   inline table / dotted key / """ multiline / hex-octal-bin int / \uXXXX escape / datetime.
//! 仍保留 our internal flat model（Doc/Table/Value）：pref.rs 校验层 + 友好 Issue 消息依赖 on it.
//!   nested values（inline table / dotted-key subtree / datetime）→ explicit TomlError
//!   （this feeds schema never uses them; the message names the offending value type）.
//! crate Table 是 alphabetical & 无位置 info → header line 由 re-scan（header_lines）.
//! crate Error 的 byte span → 1-based line number（friendly CLI message）.
//! 写入：render_block via crate serializer；feeds add 仍 append（保留 user comments）.

use toml::Table as CrateTable;
use toml::Value as CrateValue;
use toml::from_str as crate_from_str;
use toml::to_string_pretty;

#[derive(Clone)]
pub struct Doc {
    pub tables: Vec<Table>,
}

#[derive(Clone)]
pub struct Table {
    pub name: String,
    pub array: bool,
    pub line: usize,
    pub keys: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
pub struct TomlError {
    pub line: usize,
    pub message: String,
}

#[derive(Clone)]
pub enum Value {
    Str(String),
    Int(i64),
    Flt(f64),
    Bool(bool),
    Arr(Vec<Value>),
}

/// crate Value → our flat Value（nested table / datetime 明确报错）.
pub fn crate_to_value(v: &CrateValue, key: &str) -> Result<Value, TomlError> {
    if v.is_str() { return Ok(Value::Str(v.as_str().unwrap().to_string())); }
    if v.is_integer() { return Ok(Value::Int(v.as_integer().unwrap())); }
    if v.is_float() { return Ok(Value::Flt(v.as_float().unwrap())); }
    if v.is_bool() { return Ok(Value::Bool(v.as_bool().unwrap())); }
    if v.is_array() {
        let items = v.as_array().unwrap();
        let mut out = Vec::new();
        for it in items {
            match crate_to_value(&it, key) {
                Err(e) => { return Err(e); }
                Ok(x) => { out.push(x); }
            }
        }
        return Ok(Value::Arr(out));
    }
    return Err(TomlError { line: 1, message: format!("value under key {} is outside this flat schema: {}", key, v.type_str()) });
}

/// crate Table entries → our key list（crate 的 alphabetical order）.
pub fn crate_entries(tbl: &CrateTable) -> Result<Vec<(String, Value)>, TomlError> {
    let mut out = Vec::new();
    for pair in tbl.iter() {
        let (k, v) = pair;
        match crate_to_value(v, k) {
            Err(e) => { return Err(e); }
            Ok(x) => { out.push((k.to_string(), x)); }
        }
    }
    return Ok(out);
}

/// crate 的 Table 无位置 info → re-scan header 的 1-based line（friendly messages 用）.
pub fn header_lines(text: &str, name: &str, array: bool) -> Vec<usize> {
    let want = if array { format!("[[{}]]", name) } else { format!("[{}]", name) };
    let mut out = Vec::new();
    let mut line_no: usize = 0;
    for raw in text.lines() {
        line_no += 1;
        let t = raw.trim();
        if !t.starts_with(&want) { continue; }
        let rest: &str = &t[want.len()..];
        let rest_t = rest.trim();
        if rest_t.is_empty() || rest_t.starts_with("#") { out.push(line_no); }
    }
    return out;
}

/// parse via official crate; crate byte span → 1-based line.
pub fn parse(text: &str) -> Result<Doc, TomlError> {
    let tbl = match crate_from_str::<CrateTable>(&text) {
        Err(e) => {
            let line = match e.span() {
                None => { 1 }
                Some(sp) => {
                    let start = sp.start;
                    let bytes = text.as_bytes();
                    let cap = if start < bytes.len() { start } else { bytes.len() };
                    let mut n: usize = 1;
                    let mut i: usize = 0;
                    while i < cap {
                        if bytes[i] == 0x0A { n += 1; }
                        i += 1;
                    }
                    n
                }
            };
            return Err(TomlError { line: line, message: e.message().to_string() });
        },
        Ok(t) => { t }
    };
    let mut doc = Doc { tables: Vec::new() };
    for pair in tbl.iter() {
        let (k, v) = pair;
        let nm = k.to_string();
        if v.is_table() {
            let inner = v.as_table().unwrap();
            match crate_entries(inner) {
                Err(e) => { return Err(e); }
                Ok(keys) => {
                    let lines = header_lines(text, &nm, false);
                    let ln = if lines.len() > 0 { lines[0] } else { 1 };
                    doc.tables.push(Table { name: nm, array: false, line: ln, keys: keys });
                }
            }
            continue;
        }
        if v.is_array() {
            let items = v.as_array().unwrap();
            let lines = header_lines(text, &nm, true);
            let mut i: usize = 0;
            for it in items {
                if !it.is_table() {
                    return Err(TomlError { line: 1, message: format!("array of non-table values under {} is outside this schema", nm) });
                }
                let inner = it.as_table().unwrap();
                match crate_entries(inner) {
                    Err(e) => { return Err(e); }
                    Ok(keys) => {
                        let ln = if i < lines.len() { lines[i] } else { 1 };
                        doc.tables.push(Table { name: nm.clone(), array: true, line: ln, keys: keys });
                    }
                }
                i += 1;
            }
            continue;
        }
        match crate_to_value(v, &nm) {
            Err(e) => { return Err(e); }
            Ok(x) => {
                let mut keys = Vec::new();
                keys.push((nm, x));
                doc.tables.push(Table { name: "".to_string(), array: false, line: 1, keys: keys });
            }
        }
    }
    return Ok(doc);
}

/// our Value → crate Value（for the crate serializer）.
pub fn to_crate_value(v: &Value) -> CrateValue {
    match v {
        Value::Str(s) => { return CrateValue::String(s.to_string()); }
        Value::Int(i) => { return CrateValue::Integer(*i); }
        Value::Flt(f) => { return CrateValue::Float(*f); }
        Value::Bool(b) => { return CrateValue::Boolean(*b); }
        Value::Arr(xs) => {
            let mut out = Vec::new();
            for x in xs { out.push(to_crate_value(&x)); }
            return CrateValue::Array(out);
        }
    }
}

/// render one table block via the crate serializer（array=true → [[name]]）.
pub fn render_block(name: &str, array: bool, keys: Vec<(String, Value)>) -> String {
    let mut inner = CrateTable::new();
    for pair in keys {
        let (k, v) = pair;
        inner.insert(k.to_string(), to_crate_value(&v));
    }
    let mut root = CrateTable::new();
    if array {
        root.insert(name.to_string(), CrateValue::Array(vec![CrateValue::Table(inner)]));
    } else {
        root.insert(name.to_string(), CrateValue::Table(inner));
    }
    match to_string_pretty(&root) {
        Ok(s) => { return s; }
        Err(e) => { return format!("(serializer error: {:?})", e); }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::toml::parse;
    use crate::config::toml::render_block;
    use crate::config::toml::Value;

    #[test]
    fn parse_ok() {
        let text = "[default]\nengine = \"auto\"\nweight = 1.0\n\n[[feeds]]\nurl = \"https://a\"\nkeywords = [\"x\", \"y\"]\n";
        let doc = parse(&text).unwrap();
        assert_eq!(doc.tables.len(), 2);
        assert_eq!(doc.tables[1].array, true);
        assert_eq!(doc.tables[1].keys.len(), 2);
    }

    #[test]
    fn bad_value_is_friendly() {
        let r = parse("[default]\nweight = heavy\n");
        assert!(r.is_err());
    }

    #[test]
    fn comment_inside_string_kept() {
        let doc = parse("[default]\nnote = \"a # b\"\n").unwrap();
        assert_eq!(doc.tables[0].keys.len(), 1);
    }

    #[test]
    fn numeric_underscores() {
        let doc = parse("[default]\nmax_age_hours = 4_8\n").unwrap();
        assert_eq!(doc.tables[0].keys.len(), 1);
    }

    #[test]
    fn spec_values_now_accepted() {
        let doc = parse("[default]\nu = \"\\u4e7d\"\nh = 0xFF\nm = \"\"\"\nx\ny\"\"\"\n").unwrap();
        assert_eq!(doc.tables[0].keys.len(), 3);
    }

    #[test]
    fn rendered_block_round_trips() {
        let mut keys = Vec::new();
        keys.push(("url".to_string(), Value::Str("https://a".to_string())));
        keys.push(("keywords".to_string(), Value::Arr(vec![Value::Str("x".to_string())])));
        keys.push(("weight".to_string(), Value::Flt(2.5)));
        let block = render_block("feeds", true, keys);
        let back = parse(&block).unwrap();
        assert_eq!(back.tables.len(), 1);
        assert_eq!(back.tables[0].array, true);
        assert_eq!(back.tables[0].keys.len(), 3);
    }
}