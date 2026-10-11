//! 阶段 F — 存储层（RAG 知识库）。
//!
//! 两条入库路径统一到一个 JSONL 知识库：
//!   1. GUI 浏览器路径（gui crate 的 store 包装 -> 本模块）：用户浏览 / AJAX 终态正文
//!   2. HTTP 客户端路径（fetch / feed / html / search 的 --store 开关）：RSS/Atom/HTML/纯文本/SERP 摘要
//! 每条记录都写入库时间 stored_at_ms；TTL 默认 30 天，超期数据在 open / purge 时原子重写丢弃。
//!
//!   F1 doc/jsonl 文档模型（schema 版本 v）+ JSONL append + 原子 rewrite
//!   F2 fingerprint URL hash + 标题 shingle + 正文 simhash（近重复）
//!   F3 state 状态文件 store_state.toml：last_fetch / etag / seen_ids / engine

pub mod doc;
pub mod fingerprint;
pub mod jsonl;
pub mod kb;
pub mod state;
