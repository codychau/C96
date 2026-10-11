//! 阶段 G（精简版）索引层：**词项 = 字符码点整数** + 标题/正文权重 + BM25。
//!
//! 刻意不做（按"先不做过多内容"）：倒排持久化 / mmap / delta-varint 压缩 / 词位置与短语索引。
//! 词项约定：
//!   - CJK / 假名 / 谚文  -> 直接用字符码点整数（如 '卡' = 0x5361）；
//!   - ASCII 词（>=2 字） -> 字符码点的 31 进制多项式近似 + 基址（避开码点区间）。
//! 权重：标题词 ×3、正文词 ×1（文档长度用加权长度）；打分用 BM25(k1=1.2, b=0.75)，
//! 另加整串短语加成（标题 +3 / 正文 +1.5），补上单字词项精度不足的短板。

use std::collections::HashMap;

use serde::Serialize;

use crate::store::doc::now_ms;
use crate::store::doc::KbDoc;

/// ASCII 词项基址：与 CJK 码点区间（<= 0xFFFF）隔离。
pub const WORD_BASE: u32 = 0x11_0000;
pub const TITLE_W: f32 = 3.0;
pub const BODY_W: f32 = 1.0;
pub const K1: f32 = 1.2;
pub const B: f32 = 0.75;
const PHRASE_TITLE: f32 = 3.0;
const PHRASE_BODY: f32 = 1.5;

fn is_cjk(c: char) -> bool {
    let v = c as u32;
    return (v >= 0x4E00 && v <= 0x9FFF) || (v >= 0x3400 && v <= 0x4DBF) || (v >= 0xF900 && v <= 0xFAFF)
        || (v >= 0x3040 && v <= 0x30FF) || (v >= 0xAC00 && v <= 0xD7AF);
}

/// ASCII 词的整数近似值（码点 31 进制累加，非加密哈希，跨进程稳定）。
pub fn word_id(word: &str) -> u32 {
    let mut h: u32 = 0;
    for c in word.chars() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    return WORD_BASE + h;
}

/// 分词：字符码点整数序列（CJK）+ 词项整数（ASCII 词，长度 >= 2）。
pub fn terms(text: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut word = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            word.push(c);
            continue;
        }
        if word.chars().count() >= 2 { out.push(word_id(&word)); }
        word.clear();
        if is_cjk(c) { out.push(c as u32); }
    }
    if word.chars().count() >= 2 { out.push(word_id(&word)); }
    return out;
}

#[derive(Clone)]
pub struct DocMeta {
    pub url: String,
    pub title: String,
    pub host: String,
    pub source: String,
    pub stored_at_ms: u64,
    pub text: String,
}

pub struct Posting {
    pub doc: u32,
    pub w: f32,
}

#[derive(Clone, Serialize)]
pub struct IndexStats {
    pub docs: usize,
    pub terms: usize,
    pub postings: usize,
    pub avg_len: f32,
    pub build_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct Hit {
    pub doc: usize,
    pub score: f32,
    pub phrase: bool,
}

pub struct Index {
    postings: HashMap<u32, Vec<Posting>>,
    docs: Vec<DocMeta>,
    dl: Vec<f32>,
    avg_dl: f32,
    build_ms: u64,
}

impl Index {
    pub fn build(docs: &[KbDoc]) -> Index {
        let mut idx = Index::build_texts(docs.iter().map(|d| (d.url.as_str(), d.title.as_str(), d.text.as_str())));
        // build_texts 看不到 source / host / 入库时间，用 KB 记录补齐（KbHit 要用）
        for (i, d) in docs.iter().enumerate() {
            if let Some(m) = idx.docs.get_mut(i) {
                m.source = d.source.clone();
                m.host = d.host.clone();
                m.stored_at_ms = d.stored_at_ms;
            }
        }
        return idx;
    }

    /// 构建（url, title, text）。合成数据 bench 也走这里。
    pub fn build_texts<'a, I: Iterator<Item = (&'a str, &'a str, &'a str)>>(items: I) -> Index {
        let t0 = now_ms();
        let mut postings: HashMap<u32, Vec<Posting>> = HashMap::new();
        let mut metas: Vec<DocMeta> = Vec::new();
        let mut dl: Vec<f32> = Vec::new();
        for (url, title, text) in items {
            let mut tf: HashMap<u32, f32> = HashMap::new();
            for t in terms(title) { *tf.entry(t).or_insert(0.0) += TITLE_W; }
            for t in terms(text) { *tf.entry(t).or_insert(0.0) += BODY_W; }
            let mut len = 0.0f32;
            for (t, w) in tf {
                len += w;
                postings.entry(t).or_insert_with(Vec::new).push(Posting { doc: metas.len() as u32, w });
            }
            metas.push(DocMeta {
                url: url.to_string(),
                title: title.to_string(),
                host: host_of(url),
                source: String::new(),
                stored_at_ms: 0,
                text: text.to_string(),
            });
            dl.push(len.max(1.0));
        }
        let avg_dl = if dl.is_empty() { 1.0 } else { dl.iter().sum::<f32>() / dl.len() as f32 };
        return Index { postings, docs: metas, dl, avg_dl, build_ms: now_ms().saturating_sub(t0) };
    }

    pub fn doc(&self, i: usize) -> &DocMeta {
        return &self.docs[i];
    }

    pub fn stats(&self) -> IndexStats {
        let postings: usize = self.postings.values().map(|v| v.len()).sum();
        return IndexStats {
            docs: self.docs.len(),
            terms: self.postings.len(),
            postings: postings,
            avg_len: self.avg_dl,
            build_ms: self.build_ms,
        };
    }

    /// BM25 + 短语加成，返回按分数降序的 doc id。
    /// 性能要点：分数用扁平 Vec<f32> 累加（不哈希）；短语加成只在前若干名做，
    /// 避免对全部候选做 to_lowercase() 分配（这是 10k 文档下最大的开销）。
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let qs = terms(query);
        let n = self.docs.len();
        if qs.is_empty() || n == 0 || limit == 0 { return Vec::new(); }
        let nf = n as f32;
        let inv_avg = 1.0 / self.avg_dl;
        let mut scores: Vec<f32> = vec![0.0; n];
        let mut touched: Vec<u32> = Vec::new();
        for t in &qs {
            let post = match self.postings.get(t) {
                Some(p) => p,
                None => continue,
            };
            let df = post.len() as f32;
            let idf_k = (((nf - df + 0.5) / (df + 0.5)) + 1.0).ln() * (K1 + 1.0);
            for p in post.iter() {
                let i = p.doc as usize;
                // 加权 tf 恒为正 -> scores[i] == 0 就是"该文档还没被碰过"
                if scores[i] == 0.0 { touched.push(p.doc); }
                let dl = self.dl[i];
                scores[i] += idf_k * p.w / (p.w + K1 * (1.0 - B + B * dl * inv_avg));
            }
        }
        if touched.is_empty() { return Vec::new(); }
        let mut hits: Vec<Hit> = Vec::with_capacity(touched.len());
        for d in touched {
            let i = d as usize;
            hits.push(Hit { doc: i, score: scores[i], phrase: false });
        }
        hits.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
        // 短语加成只考察前 probe_n 名（加成是正数，不会让后缀反超）
        let phrase = query.trim().to_lowercase();
        if !phrase.is_empty() {
            let probe_n = limit.saturating_mul(8).max(64).min(hits.len());
            for h in hits[..probe_n].iter_mut() {
                let m = &self.docs[h.doc];
                if m.title.to_lowercase().contains(phrase.as_str()) {
                    h.score += PHRASE_TITLE;
                    h.phrase = true;
                } else if m.text.to_lowercase().contains(phrase.as_str()) {
                    h.score += PHRASE_BODY;
                    h.phrase = true;
                }
            }
            hits[..probe_n].sort_unstable_by(|a, b| {
                return b.score.total_cmp(&a.score).then(self.docs[b.doc].stored_at_ms.cmp(&self.docs[a.doc].stored_at_ms));
            });
        }
        hits.truncate(limit);
        return hits;
    }
}

fn host_of(url: &str) -> String {
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let end = rest.find('/').unwrap_or(rest.len());
    return rest[..end].to_string();
}

#[cfg(test)]
mod tests {
    use super::terms;
    use super::word_id;
    use super::Index;
    use super::WORD_BASE;
    use crate::store::kb::Store;
    use std::path::PathBuf;

    #[test]
    fn terms_use_char_codes() {
        let t = terms("卡普空 Capcom!");
        assert!(t.contains(&('卡' as u32)), "CJK 直接用码点");
        assert!(t.contains(&('普' as u32)));
        assert!(t.contains(&('空' as u32)));
        assert!(t.contains(&word_id("capcom")), "ASCII 词 -> 码点多项式近似");
        assert!(terms("Rust")[0] >= WORD_BASE);
        assert_eq!(terms("Rust"), terms(" rust "), "大小写/空白归一");
        assert!(terms("a 1").is_empty(), "单字符词与纯数字丢弃");
        assert!(terms("   ").is_empty());
    }

    #[test]
    fn title_weight_and_bm25() {
        let mut docs: Vec<(String, String, String)> = Vec::new();
        docs.push(("https://a/".to_string(), "卡普空新作".to_string(), "普通正文".to_string()));
        docs.push(("https://b/".to_string(), "普通标题".to_string(), "卡普空出现在正文里".to_string()));
        let refs: Vec<(&str, &str, &str)> = docs.iter().map(|d| (d.0.as_str(), d.1.as_str(), d.2.as_str())).collect();
        let idx = Index::build_texts(refs.into_iter());
        let hits = idx.search("卡普空", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc, 0, "标题命中权重 3 应排前");
        assert!(hits[0].phrase && hits[0].score > hits[1].score);
        let st = idx.stats();
        assert_eq!(st.docs, 2);
        assert!(st.terms > 0 && st.postings >= st.terms);
        assert!(st.avg_len > 1.0);
    }

    #[test]
    fn idf_prefers_rare_term() {
        let mut docs: Vec<(String, String, String)> = Vec::new();
        docs.push(("https://rare/".to_string(), "标题".to_string(), "zelda 独占内容".to_string()));
        for i in 0..9 {
            docs.push((format!("https://c{}/", i), "普通".to_string(), "game game game game game game game game game game".to_string()));
        }
        let refs: Vec<(&str, &str, &str)> = docs.iter().map(|d| (d.0.as_str(), d.1.as_str(), d.2.as_str())).collect();
        let idx = Index::build_texts(refs.into_iter());
        let hits = idx.search("zelda game", 5);
        assert_eq!(hits[0].doc, 0, "稀有词 idf 高，应压过高频词堆砌");
        assert!(!hits[0].phrase);
    }

    #[test]
    fn store_index_cache_invalidates_on_save() {
        let p = PathBuf::from("_scratch").join("f_index_cache.jsonl");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(crate::store::kb::default_state_path(&p));
        let mut s = Store::open(&p, 30).unwrap();
        assert_eq!(s.search_index().stats().docs, 0);
        s.save("https://x.example.com/1", "卡普空资讯", "正文 卡普空 游戏 发布", "", "manual", 0).unwrap();
        assert_eq!(s.search_index().stats().docs, 1, "save 后索引必须失效重建");
        let hits = s.search_index().search("卡普空 游戏", 5);
        assert_eq!(hits.len(), 1);
        assert!(s.search_index().doc(hits[0].doc).url.contains("x.example.com"));
        let _ = std::fs::remove_file("_scratch/f_index_cache.state.toml");
        let _ = std::fs::remove_file(&p);
    }
}
