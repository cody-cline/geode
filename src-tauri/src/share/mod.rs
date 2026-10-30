//! Sharing a setup with somebody else.
//!
//! Three paths, sharing one format.
//!
//! A **personal pack** is a file. You export a `.geodepack` and hand it over
//! however you already hand files to people. Geode uploads it nowhere and
//! hosts nothing.
//!
//! A **server** hands its own setup to the people who play on it, straight
//! from the admin's machine. Players already need the server's address to
//! join it, so there is nothing to hide by routing the pack anywhere else: the
//! admin copies a `geode://join` link, a player opens it, and their Geode
//! connects to the admin's directly and imports the pack. No service in the
//! middle, no account, and no storage anywhere but the two machines involved.
//!
//! That link carries the share code, which is the secret that lets a player
//! in — bots scan the whole internet for open ports every few hours, and the
//! pack can hold a server's private jars — and the fingerprint of the admin's
//! certificate, so a player's Geode will only ever download jars from the
//! machine the admin meant.
//!
//! A **pack at an address the sharer chose** is the third shape, for the
//! same case with no machine of theirs online: they put the file where they
//! already put files, and `geode://pack?url=…&h=…` carries the address and
//! the hash. Geode hosts nothing and holds no account, so there is no host it
//! depends on. [`packlink`] is that link, which carries where the bytes are
//! and the SHA-256 they must hash to and nothing else; [`fetch`] downloads
//! them with no credentials at all and verifies that hash before the importer
//! sees the archive.
//!
//! The rule underneath all three paths is that **bytes are a last resort**. A mod
//! that exists on a provider is carried as its reference and hashes, not as
//! itself, which keeps a pack small, keeps its mods current, and keeps Geode
//! out of the business of redistributing other people's work.

pub mod code;
pub mod fetch;
pub mod host;
pub mod identity;
pub mod join;
pub mod link;
pub mod manifest;
pub mod pack;
pub mod packlink;
pub mod portmap;
pub mod service;
pub mod trust;
