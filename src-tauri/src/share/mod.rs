//! Sharing a setup with somebody else.
//!
//! Two paths, and they share a format. One is a file you send: a pack that
//! rebuilds an instance exactly, artwork and settings included. The other is a
//! server handing its own setup to the people who play on it, so that adding a
//! mod on the server is something every client picks up.
//!
//! The rule underneath both is that **bytes are a last resort**. A mod that
//! exists on Modrinth is carried as its URL and hashes, not as itself — which
//! keeps a pack small, keeps it current, and keeps Geode out of the business of
//! redistributing other people's work. Files are only embedded when there is no
//! link to give: a hand-dropped jar, or a config the author wrote themselves.

pub mod manifest;
pub mod pack;
