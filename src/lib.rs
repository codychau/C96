//! C96 核心库（阶段 A–F）：配置 / 抓取 / 搜索 / 存储。
//!
//! 作为 lib target 暴露，供 GUI crate（gui/）复用存储层，
//! 保证两条入库路径（GUI 浏览器、HTTP 客户端）写同一个知识库文件。

pub mod config;
pub mod embed;
pub mod fetch;
pub mod index;
pub mod research;
pub mod rss;
pub mod search;
pub mod store;
