//! `zalo-core` — Headless protocol, cryptography, session, and storage engine for `litechat`.
//!
//! Strictly decoupled from any GUI toolkit or local unauthenticated HTTP servers.

pub mod chat;
pub mod crypto;
pub mod data_dir;
pub mod dpapi;
pub mod endpoints;
pub mod login;
pub mod param_cipher;
pub mod qr;
pub mod relay;
pub mod session;
pub mod socket;
pub mod store;
pub mod translate;
