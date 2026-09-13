//! Sharing a setup with somebody else.
//!
//! Two paths, sharing one format, and they are deliberately different shapes.
//!
//! A **personal pack** is a file. You export a `.geodepack` and hand it over
//! however you already hand files to people — a chat message, a USB stick, your
//! own cloud drive. Geode uploads it nowhere and hosts nothing. That was a
//! deliberate narrowing: every free host that needs no account either caps at
//! about a hundred megabytes, gates downloads behind a page with short-lived
//! signed links, or refuses archives outright, and the ones with real capacity
//! all want an account per person. Asking every user to provision storage so
//! that Geode can hand them back a file they could have sent themselves is a
//! worse product than just giving them the file.
//!
//! A **server** is the other case, and it is the one worth the machinery. A
//! server is already a machine that stays on, so it can hand its own setup
//! straight to the people who play on it: the admin shares, players fetch, and
//! nothing is stored anywhere in between. Adding a mod on the server is then
//! something every client picks up.
//!
//! The rule underneath both is that **bytes are a last resort**. A mod that
//! exists on a provider is carried as its reference and hashes, not as itself —
//! which keeps a pack small, keeps its mods current, and keeps Geode out of the
//! business of redistributing other people's work. Files are only embedded when
//! there is no link to give: a config the author wrote, or a jar with no
//! provenance.

pub mod code;
pub mod manifest;
pub mod pack;
