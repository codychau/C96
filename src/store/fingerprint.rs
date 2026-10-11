//! F2 去指纹：URL hash（复用 D4）+ 标题 shingle + 正文 simhash + 汉明距离。

use crate::search::normalize::normalize_url;
use crate::search::normalize::url_hash;

/// CJK 统一表意 / 扩展 A / 兼容 + 假名 + 谚文（n-gram 特征用）。
pub fn is_cjk(c: char) -> bool {
    let u = c as u32;
    if u >= 0x4E00 && u <= 0x9FFF { return true; }
    if u >= 0x3400 && u <= 0x4DBF { return true; }
    if u >= 0xF900 && u <= 0xFAFF { return true; }
    if u >= 0x3040 && u <= 0x30FF { return true; }
    if u >= 0xAC00 && u <= 0xD7AF { return true; }
    return false;
}

pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    return h;
}

pub fn hex64(v: u64) -> String {
    return format!("{:016x}", v);
}

/// URL 去重键：normalize 后的 FNV hash（与 D4 dedup 完全一致）。
pub fn url_key(url: &str) -> String {
    return url_hash(&normalize_url(url));
}

/// 特征序列：ASCII 词（小写）+ CJK 二元组。
pub fn features(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut word = String::new();
    let mut i: usize = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        } else if !word.is_empty() {
            out.push(word.clone());
            word.clear();
        }
        if is_cjk(c) && i + 1 < chars.len() && is_cjk(chars[i + 1]) {
            let mut g = String::new();
            g.push(c);
            g.push(chars[i + 1]);
            out.push(g);
        }
        i += 1;
    }
    if !word.is_empty() { out.push(word); }
    if out.len() > 4000 { out.truncate(4000); }
    return out;
}

/// 标题 shingle：特征集合排序去重后 hash（顺序 / 大小写 / 标点不敏感）。
pub fn title_shingle(title: &str) -> u64 {
    let mut f = features(title);
    if f.is_empty() { return 0; }
    f.sort();
    f.dedup();
    return fnv1a64(&f.join("\u{1f}"));
}

/// 64-bit simhash：每个特征 hash 对 64 个 bit 投票。
pub fn simhash(text: &str) -> u64 {
    let f = features(text);
    if f.is_empty() { return 0; }
    let mut bits: [i64; 64] = [0; 64];
    for ft in f {
        let h = fnv1a64(&ft);
        let mut i: usize = 0;
        while i < 64 {
            if (h >> i) & 1 == 1 { bits[i] += 1; } else { bits[i] -= 1; }
            i += 1;
        }
    }
    let mut out: u64 = 0;
    let mut i: usize = 0;
    while i < 64 {
        if bits[i] > 0 { out |= 1u64 << i; }
        i += 1;
    }
    return out;
}

/// 汉明距离（不同 bit 个数）。
pub fn hamming(a: u64, b: u64) -> u32 {
    let mut x = a ^ b;
    let mut n: u32 = 0;
    while x != 0 {
        n += 1;
        x &= x - 1;
    }
    return n;
}

#[cfg(test)]
mod tests {
    use super::features;
    use super::hamming;
    use super::hex64;
    use super::simhash;
    use super::title_shingle;
    use super::url_key;

    #[test]
    fn hash_is_stable_and_hex16() {
        let a = url_key("HTTPS://Example.COM:443/a/?utm_source=x#f");
        let b = url_key("https://example.com/a");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_eq!(hex64(0), "0000000000000000");
    }

    #[test]
    fn title_shingle_ignores_case_and_punctuation() {
        let a = title_shingle("Rust 编译器 - Cargo 构建!");
        let b = title_shingle("cargo 构建 rust 编译器");
        assert_eq!(a, b);
        assert_ne!(a, title_shingle("Python 解释器"));
        assert_eq!(title_shingle(""), 0);
    }

    #[test]
    fn simhash_near_duplicates_are_close() {
        let base = "Rust 是一门系统编程语言，强调安全与并发。Cargo 是 Rust 的构建系统与包管理器，负责依赖解析、编译与测试。";
        let edited = "Rust 是一门系统编程语言，强调安全与并发。Cargo 是 Rust 的构建系统与包管理器，负责依赖解析、编译与发布。";
        let other = "今天天气不错，适合去公园散步并且拍照留念。";
        let a = simhash(base);
        let b = simhash(edited);
        let c = simhash(other);
        assert_eq!(hamming(a, a), 0);
        assert!(hamming(a, b) <= 16, "near distance {}", hamming(a, b));
        assert!(hamming(a, c) > 16, "far distance {}", hamming(a, c));
    }

    #[test]
    fn features_mix_ascii_and_cjk() {
        let f = features("Rust 编译器 cargo");
        assert!(f.contains(&"rust".to_string()));
        assert!(f.contains(&"cargo".to_string()));
        let g = features("编译器");
        assert_eq!(g.len(), 2);
        assert_eq!(simhash(""), 0);
    }
}
