//! Luxforge's update feed: the single home of its format, shared by xtask, which signs archives
//! and writes records and feeds, and the Linux updater, which reads feeds and verifies archives.
//!
//! - [`UpdateRecord`]: a release's `luxforge-<version>-updates.json`, holding each archive's
//!   name, length, build number, Ed25519 signature, minimum OS, catalog format and signing status.
//! - [`Appcast`]: the Sparkle-format RSS appcast, one per [`Target`], written by
//!   [`feed::generate`] from the records of every published release and read by
//!   [`Appcast::parse`].
//! - [`SigningKey`], [`PublicKey`] and [`Signature`]: Ed25519 in Sparkle's `sparkle:edSignature`
//!   form, a file signed or verified through a memory map rather than read onto the heap.
//! - [`newest_eligible`]: the update a feed offers a build that follows some channels.
//! - [`UpdateConfig`]: `release/update.json`, the feed base URL and the public key.
//!
//! It depends on no other workspace crate and on no GUI or network crate.
mod appcast;
mod config;
mod date;
mod error;
pub mod feed;
mod key;
mod record;
mod select;
mod target;

pub use appcast::{
    Appcast, AppcastItem, ENCLOSURE_TYPE, Enclosure, LUXFORGE_NAMESPACE, MAX_APPCAST_BYTES,
    ParsedAppcast, SPARKLE_NAMESPACE, SkippedItem,
};
pub use config::{CONFIG_PATH, UpdateConfig, WITHDRAWN_PATH};
pub use date::pub_date;
pub use error::{Error, Result};
pub use key::{MAX_PRIVATE_KEY_TEXT, PublicKey, Signature, SigningKey};
pub use record::{
    ArchiveRecord, MAX_RECORD_BYTES, RECORD_SCHEMA, SigningStatus, UpdateRecord, tag_version,
};
pub use select::newest_eligible;
pub use target::{Channel, Target};

#[cfg(test)]
mod tests;
