//! 阶段 C — 轻量抓取层（RSS-like 核心，无反爬站点）。
//!   C1 http：UA / timeout / retry / conditional GET（ETag + Last-Modified）
//!   C2 feed：RSS 2.0 + Atom -> unified Article（xml-rs 0.8.29，离线 crate）。
//!   C3 html：DOM-lite + link/text density 正文抽取（strip script/style，剔 boilerplate）。

pub mod http;
pub mod cache;
pub mod feed;
pub mod html;
