//! The `/api` that FLE Studio's web page talks to, in Rust. Stage 0 grows route by route; the Python Studio is the
//! specification and `tests/` compare both servers request by request.
#![allow(clippy::manual_is_multiple_of)] // `is_multiple_of` needs a newer Rust than we promise
pub mod agent;
pub mod bench;
pub mod cachewatch;
pub mod catalog;
pub mod chats;
pub mod demo;
pub mod downloads;
pub mod engine;
pub mod engines;
pub mod flaghelp;
pub mod forge;
pub mod found;
pub mod gguf;
pub mod github;
pub mod hf;
pub mod launch;
pub mod library;
pub mod live;
pub mod mcp;
pub mod oauth;
pub mod planview;
pub mod quality;
pub mod remote;
pub mod report_time;
pub mod run;
pub mod server;
pub mod settings;
pub mod stats;
pub mod system;
pub mod tls;
pub mod tools;
pub mod tuner;
pub mod updates;
