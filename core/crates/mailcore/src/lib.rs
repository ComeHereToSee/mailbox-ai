//! Mailbox core: account sync (IMAP / POP3 / SMTP), local store, search and the
//! local HTTP API used by the macOS app.

pub mod ai;
pub mod api;
pub mod autoconfig;
pub mod categorize;
pub mod crypto;
pub mod engine;
pub mod error;
pub mod html;
pub mod imap;
pub mod events;
pub mod forward;
pub mod ics;
pub mod mime;
pub mod model;
pub mod net;
pub mod oauth;
pub mod paths;
pub mod pop3;
pub mod rules;
pub mod search;
pub mod secrets;
pub mod sieve;
pub mod smtp;
pub mod store;

pub use engine::Engine;
pub use error::{Error, Result};
