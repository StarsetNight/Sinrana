//! Sinrana 参考服务器库。
//!
//! 该 crate 将服务器逻辑作为库暴露出来，以便集成测试可以端到端地驱动它，
//! 同时还提供了一个 `sinrana-server` 二进制程序。

pub mod auth;
pub mod cert;
pub mod config;
pub mod db;
pub mod server;

#[cfg(feature = "web")]
pub mod web;

pub use config::Config;
pub use server::Server;
