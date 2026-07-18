//! Remindi's production process shell and shared application boundaries.

pub mod admin;
pub mod app;
pub mod auth;
pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod mcp;
pub mod remindi;
pub mod scheduler;
pub mod triggers;
pub mod webui;

#[path = "http/api/admin.rs"]
pub mod admin_http_api;
