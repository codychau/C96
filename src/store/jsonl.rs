//! F1 JSONL 读写：追加行 + 原子整体重写（tmp -> rename）。

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::store::doc::KbDoc;

pub fn ensure_parent(path: &Path) -> bool {
    let p = match path.parent() {
        Some(x) => x,
        None => { return true; }
    };
    if p.as_os_str().is_empty() { return true; }
    return fs::create_dir_all(p).is_ok();
}

/// 追加一行（补换行 + flush）；父目录不存在则创建。
pub fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if !ensure_parent(path) { return Err(format!("cannot create parent dir for {}", path.display())); }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    f.write_all(&[10u8]).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())?;
    return Ok(());
}

/// 读取非空行（缺文件 = 空）。
pub fn read_lines(path: &Path) -> Vec<String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => { return Vec::new(); }
    };
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        if !l.trim().is_empty() { out.push(l.to_string()); }
    }
    return out;
}

/// 原子重写：写 path.tmp 再 rename，替换整个文件。
pub fn rewrite_atomic(path: &Path, lines: &[String]) -> Result<(), String> {
    if !ensure_parent(path) { return Err(format!("cannot create parent dir for {}", path.display())); }
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    let tmp = path.to_string_lossy().to_string() + ".tmp";
    fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    return Ok(());
}

/// 一行 -> KbDoc（坏行返回 None，永不 fatal）。
pub fn parse_doc(line: &str) -> Option<KbDoc> {
    return serde_json::from_str::<KbDoc>(line).ok();
}

#[cfg(test)]
mod tests {
    use super::append_line;
    use super::parse_doc;
    use super::read_lines;
    use super::rewrite_atomic;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        return PathBuf::from("_scratch").join(name);
    }

    #[test]
    fn append_read_round_trip() {
        let p = tmp("f_jsonl_round.jsonl");
        let _ = std::fs::remove_file(&p);
        assert!(append_line(&p, "{\"v\":1,\"hash\":\"aa\",\"url\":\"https://a\",\"text\":\"one\"}").is_ok());
        assert!(append_line(&p, "not json").is_ok());
        assert!(append_line(&p, "{\"v\":1,\"hash\":\"bb\",\"url\":\"https://b\",\"text\":\"two\"}").is_ok());
        let lines = read_lines(&p);
        assert_eq!(lines.len(), 3);
        let d = parse_doc(&lines[0]);
        assert!(d.is_some());
        assert_eq!(d.as_ref().unwrap().hash, "aa");
        assert_eq!(d.as_ref().unwrap().v, 1);
        assert_eq!(d.as_ref().unwrap().text_len, 0);
        assert!(parse_doc(&lines[1]).is_none());
    }

    #[test]
    fn rewrite_atomic_replaces_and_skips_blanks() {
        let p = tmp("f_jsonl_rewrite.jsonl");
        assert!(append_line(&p, "old1").is_ok());
        assert!(append_line(&p, "old2").is_ok());
        let next = vec!["new1".to_string(), "".to_string(), "  ".to_string(), "new2".to_string()];
        assert!(rewrite_atomic(&p, &next).is_ok());
        let lines = read_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "new1");
        assert_eq!(lines[1], "new2");
        assert!(!tmp("f_jsonl_rewrite.jsonl.tmp").exists());
    }
}
