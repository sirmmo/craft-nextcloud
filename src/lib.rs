//! craft-nextcloud: Nextcloud files for the craft apps (GridCraft, WordCraft, DeckCraft), as an
//! out-of-process add-in speaking the craft plugin protocol 1.2.
//!
//! - [`plugin`]: the JSON-RPC loop (`initialize`, `commands/run`, `events/*`).
//! - [`commands`]: the ribbon's commands and the dialogs they chain.
//! - [`events`]: what happens when a document opens, is saved or closes.
//! - [`dav`]: Nextcloud over HTTP: WebDAV, Login Flow v2, OCS.
//! - [`store`]: accounts, links and settings on disk, shared by the three apps.
//! - [`backup`]: local backups and their retention.
//! - [`paths`], [`time`], [`text`]: remote and local paths, dates, translations.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod backup;
pub mod commands;
pub mod dav;
pub mod events;
pub mod manifest;
pub mod paths;
pub mod plugin;
pub mod store;
pub mod sync;
pub mod system;
pub mod text;
pub mod time;
