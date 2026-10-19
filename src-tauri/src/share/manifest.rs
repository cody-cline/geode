//! What a shared pack says about itself, beyond what a `.mrpack` can.
//!
//! A Geode pack file **is** a `.mrpack` — `modrinth.index.json`, `overrides/`,
//! `client-overrides/`, `server-overrides/`, all at the root and all valid —
//! with one extra directory, `geode/`, alongside them. Nothing is nested and
//! nothing is wrapped.
//!
//! That shape is the whole design. The index *instructs*: it holds every byte
//! another launcher needs to rebuild a working instance, so Prism or the
//! Modrinth app open a Geode pack as an ordinary modpack and get it right. The
//! manifest here only *annotates*: the launcher settings, artwork and
//! provenance that `.mrpack` has nowhere to put. Delete `geode/` and the pack
//! must still install correctly elsewhere — there is a test for exactly that.
//!
//! The corollary is a rule worth stating: never write a download URL into this
//! half. The moment the same fact lives in both places they can disagree, and
//! the index is the one other software reads.

use serde::{Deserialize, Serialize};

/// The newest layout this build understands, and so the highest a pack may
/// claim before it is refused.
///
/// Bumped when the layout changes in a way an older reader would misread.
/// Format 2 is the `disabled` note on a file the index carries: see
/// [`format_for`].
pub const FORMAT: u32 = 2;

/// The layout a reader has to understand to read a pack with these notes,
/// which is not always the newest one.
///
/// Only one thing a note can say is *misread* rather than merely missed by a
/// format-1 Geode: `disabled` on a file with no `source`. Such a file is in
/// the index, so it is fetched; a format-1 reader applies the off state only
/// to files it fetched by id, so it switches nothing off and a mod the sharer
/// turned off — usually because it crashed — arrives switched on. Claiming 2
/// makes that reader refuse the pack and say to update, which is the honest
/// answer.
///
/// Every other pack keeps saying 1, because refusing one costs the recipient
/// every mod in it, and most packs have nothing turned off.
pub fn format_for(files: &[FileNote]) -> u32 {
    if files.iter().any(|n| n.disabled && n.source.is_none()) {
        2
    } else {
        1
    }
}

/// The file extension.
///
/// Not `.geode`: that is already the Geometry Dash mod SDK's extension, and it
/// is also a zip, so the collision would be a real one rather than cosmetic.
pub const EXTENSION: &str = "geodepack";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub geode_format: u32,
    /// What was exported. A server pack carries per-file sides an instance
    /// pack has no use for.
    pub kind: Kind,
    pub profile: Profile,
    /// Where the artwork lives inside the zip, if any was exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// The modpack this was itself built from, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::instance::PackSource>,
    /// Per-file notes, keyed by the same path the index uses.
    ///
    /// Only files that need saying something about appear here; a pack where
    /// every mod runs on both sides carries an empty map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileNote>,
    /// In a server's pack, the revision of what this very pack holds, so a
    /// player keeps the revision of what they received rather than of a
    /// description read before it was built — a server changed and changed
    /// back in between would otherwise never be noticed.
    ///
    /// An instance's pack has none, and so does a server's from a Geode older
    /// than this, which an older reader ignores in turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// In a server's pack, how the server should appear in its players'
    /// multiplayer list — so a player who imports it can join from the game's
    /// own screen rather than being handed a title screen and no address.
    ///
    /// Deliberately does **not** bump [`FORMAT`]: an older Geode that ignores
    /// this adds no row, and the instance it builds is right in every other
    /// respect. That is a miss, not a misread — see [`format_for`].
    ///
    /// An instance's pack never carries one. There is no server behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<Listing>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Instance,
    Server,
}

/// How a server should appear in its players' multiplayer list.
///
/// Two things, both the admin's to choose: what players see it called, and the
/// address they connect to *for the game*. That second one is not the address
/// the pack came down — packs come down Geode's own port, the game connects to
/// the server's `server-port`, and the two may differ in host as well as port.
/// Confusing them writes a row that cannot connect, which is why the address
/// here goes through [`crate::share::link::game_address`] and never through
/// the share address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    /// Shown in the player's list. Not held to an address's printable-ASCII
    /// rule: a name legitimately holds emoji, `§` colour codes and non-Latin
    /// scripts, which is why the list writer encodes Java's own UTF-8.
    pub name: String,
    /// `play.example.com`, or `play.example.com:25566`. Minecraft's own port
    /// is left off so the string matches what a player would type by hand.
    pub address: String,
}

/// The longest [`Listing::name`] stored, in characters.
///
/// Ours, not a claim about Minecraft's own Add Server field, whose exact limit
/// is not something this build knows. It is here so a list stays readable, and
/// so a name fits an NBT string's own length whatever script it is written in.
pub const MAX_LISTING_NAME: usize = 64;

impl Listing {
    /// This listing, fit to write into somebody's multiplayer list — or why it
    /// is not.
    ///
    /// One validator for both sides, so the admin's typing and a pack off the
    /// network are held to the same rules. The admin is shown the reason; an
    /// import takes a refusal as "no row", which is the graceful degrade an
    /// older pack already gets.
    pub fn checked(&self) -> crate::error::Result<Listing> {
        use crate::error::Error;

        let name = self.name.trim();
        if name.is_empty() {
            return Err(Error::msg("give the server a name for players' server lists"));
        }
        // Control characters garble a list rather than appearing in it, and
        // a newline in an NBT string is a name that reads as two.
        if name.chars().any(|c| c.is_control()) {
            return Err(Error::msg(
                "that name has control characters in it, which would garble a player's server list",
            ));
        }
        let length = name.chars().count();
        if length > MAX_LISTING_NAME {
            return Err(Error::msg(format!(
                "that name is {length} characters — keep it to {MAX_LISTING_NAME} so it fits a player's server list"
            )));
        }
        Ok(Listing {
            name: name.to_string(),
            address: crate::share::link::game_address(&self.address)?,
        })
    }
}

/// The launcher-level settings a `.mrpack` has no field for.
///
/// All optional: a pack that does not carry a memory size should leave the
/// importer's own default alone rather than overwrite it with a zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_args: Option<String>,
    /// Deliberately **not** exported: a path on the author's machine means
    /// nothing on anybody else's, and a wrong one stops the game starting.
    /// Present only so the shape is obvious to a reader of the file.
    #[serde(skip)]
    pub java_path_never_shared: (),
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// Server software, for a server pack. Meaningless for an instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_kind: Option<String>,
}

/// Where a file came from, so a recipient can fetch it rather than be sent it.
///
/// This is the whole point of the format. A jar that exists on a provider is
/// carried as a reference and resolved by the recipient through their own
/// credentials — which keeps packs small, keeps mods current, and keeps Geode
/// out of redistributing other people's work.
///
/// Non-Modrinth references live here rather than in the index, and not because
/// the index forbids them: its four-host allowlist belongs to Modrinth's upload
/// validator, and readers do not enforce it. They live here because the index
/// can only hold a URL, and a URL baked into a pack goes on working after an
/// author turns third-party downloads off. An id resolved at import time gets
/// today's answer instead of the one that was true when somebody exported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
}

/// Something worth saying about one file in the index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileNote {
    /// The same path the index uses, so the two halves line up by key.
    pub path: String,
    /// Which side the file belongs on. In an instance's pack, what the jar
    /// declared about itself, carried across so an importer does not have to
    /// re-read every jar to find out. In a server's pack, how the server
    /// classified it — its folder, then the jar, then the listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<crate::instance::jar::Side>,
    /// Where to get it, when it is not something the index can point at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// The sharer had this turned off.
    ///
    /// A file carried as bytes keeps its `.disabled` name on its own, but a
    /// reference is fetched fresh, so without this a mod somebody switched off
    /// because it crashed would arrive switched on and crash for the recipient
    /// too. Kept apart from `path`, which stays on the enabled name so the
    /// reference and the exporter's skip set go on matching.
    ///
    /// Set for a file the *index* carries as well as one this half sources,
    /// which is what [`format_for`] exists to version: a format-1 reader
    /// honours it only in the second case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl Manifest {
    /// Where the manifest lives inside the zip.
    pub const PATH: &'static str = "geode/manifest.json";

    /// Whether this reader understands the file.
    ///
    /// A newer pack is refused rather than half-read: silently ignoring fields
    /// it does not know would import a pack that looks right and is not.
    pub fn readable(&self) -> bool {
        self.geode_format <= FORMAT
    }

    pub fn note(&self, path: &str) -> Option<&FileNote> {
        self.files.iter().find(|f| f.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile {
                name: "Test".into(),
                memory_mb: Some(4096),
                ..Default::default()
            },
            icon: None,
            banner: None,
            source: None,
            files: vec![FileNote {
                path: "mods/sodium.jar".into(),
                side: Some(crate::instance::jar::Side::Client),
                source: None,
                disabled: false,
            }],
            revision: None,
            listing: None,
        }
    }

    #[test]
    fn a_manifest_round_trips() {
        let json = serde_json::to_string(&manifest()).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest());
    }

    /// The absent things must stay absent rather than serialising as nulls: a
    /// pack that carries `"group": null` is asking an importer to clear a
    /// group the user set, which is not what "I did not export one" means.
    #[test]
    fn nothing_optional_is_written_when_it_is_absent() {
        let json = serde_json::to_string(&manifest()).unwrap();
        for absent in ["group", "banner", "source", "width", "serverKind", "listing"] {
            assert!(!json.contains(absent), "{absent} should not be written: {json}");
        }
        // And a file with nothing to say about it carries no flag either.
        assert!(!json.contains("disabled"));
        assert!(!json.contains("source"));
    }

    /// A file a pack cannot carry travels as a reference instead, and the note
    /// is the whole of what the recipient has to go on: the provider, project
    /// and version it survives the round trip with are what they fetch it by.
    #[test]
    fn a_file_that_cannot_be_redistributed_is_referenced_not_embedded() {
        let note = FileNote {
            path: "mods/private.jar".into(),
            side: None,
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "12345".into(),
                version_id: "67890".into(),
            }),
            disabled: false,
        };
        let json = serde_json::to_string(&note).unwrap();
        assert!(json.contains("curseforge"));
        let back: FileNote = serde_json::from_str(&json).unwrap();
        assert_eq!(back, note);
    }

    /// A path on the exporter's machine is meaningless on anyone else's, and a
    /// wrong one stops the game starting — so it is never written at all.
    #[test]
    fn a_java_path_is_never_exported() {
        let json = serde_json::to_string(&manifest()).unwrap();
        assert!(!json.to_lowercase().contains("javapath"));
    }

    /// A pack claims the oldest layout that reads it correctly, because the
    /// cost of claiming a newer one is that an older Geode refuses the pack
    /// whole and the recipient gets none of it.
    ///
    /// The one note an older reader gets *wrong* is `disabled` on a file it
    /// fetches from the index: it applies the off state only to files it
    /// resolved by id, so the mod arrives switched on. That pack says 2.
    #[test]
    fn only_a_pack_an_older_reader_would_misread_claims_the_newer_format() {
        let plain = FileNote {
            path: "mods/sodium.jar".into(),
            side: None,
            source: None,
            disabled: false,
        };
        assert_eq!(format_for(std::slice::from_ref(&plain)), 1);

        // Turned off and sourced here: an older reader writes a reference it
        // resolved straight to the off name, so nothing is misread.
        let sourced = FileNote {
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "1".into(),
                version_id: "2".into(),
            }),
            disabled: true,
            ..plain.clone()
        };
        assert_eq!(format_for(&[sourced]), 1);

        // Turned off and carried by the index: only this layout switches it
        // back off.
        let indexed_off = FileNote {
            disabled: true,
            ..plain
        };
        assert_eq!(format_for(&[indexed_off]), 2);
    }

    /// A listing is an *addition*: an older Geode that does not know the field
    /// ignores it and adds no row, and the instance it builds is correct in
    /// every other respect. Refusing such a pack would cost its recipient
    /// every mod in it to save them a row in a list.
    ///
    /// Make `format_for` return 3 for a listing and this fails — which is the
    /// mistake worth a test, since the field is new and bumping looks tidy.
    #[test]
    fn a_listing_does_not_claim_a_newer_format() {
        let plain = FileNote {
            path: "mods/sodium.jar".into(),
            side: None,
            source: None,
            disabled: false,
        };
        // `format_for` reads the file notes, and a listing is not one of them.
        assert_eq!(format_for(std::slice::from_ref(&plain)), 1);

        let listed = Manifest {
            geode_format: format_for(std::slice::from_ref(&plain)),
            kind: Kind::Server,
            listing: Some(Listing {
                name: "Survival".into(),
                address: "play.example.com".into(),
            }),
            files: vec![plain],
            ..manifest()
        };
        assert_eq!(listed.geode_format, 1, "a listing claimed a newer format");
        assert!(listed.readable());

        // And a reader with no such field reads the rest of the pack, rather
        // than refusing it — which is what "an older reader" actually does,
        // since `Manifest` denies no unknown fields.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OlderManifest {
            geode_format: u32,
            kind: Kind,
            profile: Profile,
        }
        let json = serde_json::to_string(&listed).unwrap();
        assert!(json.contains("\"listing\""), "{json}");
        let older: OlderManifest = serde_json::from_str(&json).unwrap();
        assert_eq!((older.geode_format, older.kind), (1, Kind::Server));
        assert_eq!(older.profile.name, "Test");
    }

    /// The two things the admin chooses, round-tripped, and the rules that
    /// keep a name out of a player's list when it would garble it.
    #[test]
    fn a_listing_round_trips_and_a_name_a_list_cannot_show_is_refused() {
        let listing = Listing {
            name: "  §aSurvival 🎮  ".into(),
            address: "PLAY.example.com:25565".into(),
        };
        let checked = listing.checked().unwrap();
        // Trimmed, and the address written the way a player would type it.
        assert_eq!(checked.name, "§aSurvival 🎮");
        assert_eq!(checked.address, "play.example.com");
        let back: Listing = serde_json::from_str(&serde_json::to_string(&checked).unwrap()).unwrap();
        assert_eq!(back, checked);

        let bad = |name: &str| Listing {
            name: name.into(),
            address: "play.example.com".into(),
        }
        .checked()
        .expect_err(&format!("{name:?} was accepted"))
        .to_string();

        assert!(bad("").contains("give the server a name"));
        assert!(bad("   ").contains("give the server a name"));
        assert!(bad("Survival\nNot really").contains("control characters"));
        assert!(bad("Survival\u{7f}").contains("control characters"));
        assert!(bad(&"x".repeat(MAX_LISTING_NAME + 1)).contains("characters"));
        // Right up to the cap is fine, counted in characters rather than
        // bytes so a non-Latin name is not refused for being itself.
        assert!(Listing {
            name: "あ".repeat(MAX_LISTING_NAME),
            address: "play.example.com".into()
        }
        .checked()
        .is_ok());

        // And the address goes through the one address validator.
        assert!(Listing {
            name: "Survival".into(),
            address: "https://pack.example.com".into()
        }
        .checked()
        .is_err());
    }

    #[test]
    fn a_pack_from_the_future_is_refused_rather_than_half_read() {
        let mut m = manifest();
        m.geode_format = FORMAT + 1;
        assert!(!m.readable());
        m.geode_format = FORMAT;
        assert!(m.readable());
    }
}
