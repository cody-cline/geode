//! This machine's identity as a sharer: one self-signed certificate, for life.
//!
//! No certificate authority is involved and none is needed. Players reach the
//! admin by IP, which no authority will certify, and the link they are given
//! carries this certificate's fingerprint — so the player's Geode accepts
//! exactly this certificate and nothing else, which is a stronger promise than
//! a CA would make anyway.
//!
//! Generated once and kept. Regenerating it would invalidate every link ever
//! handed out, which is why it lives beside the rest of Geode's data rather
//! than being derived from anything that might change.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::paths::Paths;

use super::link::Pin;

const CERT_FILE: &str = "identity.crt";
const KEY_FILE: &str = "identity.key";

/// The only name the certificate carries. Players check the fingerprint, not
/// the name, so this one is deliberately unresolvable and says nothing about
/// the machine to whoever connects.
const CERT_NAME: &str = "geode-share.invalid";

/// Held while deciding whether an identity exists and creating it if not.
///
/// The first share on a machine can ask for the identity twice at once — the
/// listener starting while the link is copied. Without this, both would find
/// nothing, both would generate, and the slower one would fail on the files
/// the faster one had half-written instead of simply reading them.
static CREATING: Mutex<()> = Mutex::new(());

/// A certificate and its private key, loaded from or written to
/// [`Paths::share_dir`].
pub struct Identity {
    /// DER-encoded, self-signed.
    pub cert_der: Vec<u8>,
    /// DER-encoded PKCS#8.
    pub key_der: Vec<u8>,
}

impl Identity {
    /// Read the identity from disk, creating and saving one the first time.
    ///
    /// Files: `share/identity.crt` (DER) and `share/identity.key` (DER,
    /// PKCS#8, mode 0600 on Unix). A key file that exists but will not parse
    /// is an error, never silently replaced — replacing it would break every
    /// link out there without saying why.
    pub async fn load_or_create(paths: &Paths) -> Result<Self> {
        let dir = paths.share_dir();
        tokio::task::spawn_blocking(move || load_or_create_in(&dir))
            .await
            .map_err(|e| Error::msg(format!("loading the sharing certificate panicked: {e}")))?
    }

    /// What goes in a link: SHA-256 over the certificate's DER bytes.
    pub fn pin(&self) -> Pin {
        Pin(Sha256::digest(&self.cert_der).into())
    }

    /// A rustls server configuration presenting this certificate, on the
    /// `ring` provider, TLS 1.2 and 1.3, no client authentication.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        self.rustls_server_config()
            .map(Arc::new)
            .map_err(|e| Error::msg(format!("the sharing certificate cannot be used: {e}")))
    }

    /// Building the configuration is also the check that the files are sound:
    /// rustls parses the key, parses the certificate, and refuses a key that
    /// is not the one the certificate was made for.
    fn rustls_server_config(&self) -> std::result::Result<rustls::ServerConfig, rustls::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.cert_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone())),
            )
    }
}

fn load_or_create_in(dir: &Path) -> Result<Identity> {
    let _creating = CREATING.lock().unwrap_or_else(|p| p.into_inner());
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);

    match (read_if_present(&cert_path)?, read_if_present(&key_path)?) {
        (Some(cert_der), Some(key_der)) => {
            let identity = Identity { cert_der, key_der };
            identity.rustls_server_config().map_err(|e| {
                Error::msg(format!(
                    "this machine's sharing certificate in {} cannot be read ({e}). It has been \
                     left as it is, because a new one would stop every link already handed out \
                     from working.",
                    dir.display()
                ))
            })?;
            Ok(identity)
        }
        (None, None) => create(dir, &cert_path, &key_path),
        // Half an identity is not one to quietly finish: a lone key is still
        // the machine's secret, and a lone certificate is what every link out
        // there names. Either way the person who can fix it needs to know.
        (Some(_), None) => Err(half_missing(&key_path, &cert_path)),
        (None, Some(_)) => Err(half_missing(&cert_path, &key_path)),
    }
}

fn create(dir: &Path, cert_path: &Path, key_path: &Path) -> Result<Identity> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![CERT_NAME.to_string()])
            .map_err(|e| Error::msg(format!("could not generate a sharing certificate: {e}")))?;
    let identity = Identity {
        cert_der: cert.der().to_vec(),
        key_der: signing_key.serialize_der(),
    };

    std::fs::create_dir_all(dir)?;
    write_new(key_path, &identity.key_der, 0o600)?;
    if let Err(e) = write_new(cert_path, &identity.cert_der, 0o644) {
        // The key was written a moment ago by this call and nothing has seen
        // it. Left behind, it would make every later launch refuse to start
        // sharing over an identity that never existed.
        let _ = std::fs::remove_file(key_path);
        return Err(e.into());
    }
    Ok(identity)
}

/// Write a file that must not exist yet, with its permissions set as it is
/// created — never widened first and narrowed after, which leaves a moment
/// where the key is readable by anyone.
///
/// Synced before returning because a link can be handed out the moment this
/// succeeds, and a crash that left an empty key behind would be refused on
/// every launch after it.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;

    let mut file = options.open(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .inspect_err(|_| {
            let _ = std::fs::remove_file(path);
        })
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn half_missing(missing: &Path, remaining: &Path) -> Error {
    Error::msg(format!(
        "{} is missing from this machine's sharing identity. Restore it from a backup, or \
         delete {} as well to make a new identity — which stops every link already handed out \
         from working.",
        missing.display(),
        remaining.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    fn files(paths: &Paths) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<_> = std::fs::read_dir(paths.share_dir())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, std::fs::read(entry.path()).unwrap())
            })
            .collect();
        out.sort();
        out
    }

    /// The whole point of keeping an identity is that the next launch
    /// presents the same certificate, so links handed out yesterday still
    /// match today.
    #[tokio::test]
    async fn an_identity_is_made_once_and_read_back_unchanged() {
        let temp = Scratch::new("identity-reload");
        let paths = &temp.paths;

        let first = Identity::load_or_create(paths).await.unwrap();
        let second = Identity::load_or_create(paths).await.unwrap();

        assert_eq!(first.pin(), second.pin());
        assert_eq!(first.cert_der, second.cert_der);
        assert_eq!(first.key_der, second.key_der);

        // The pin is over the certificate exactly as it is stored.
        let stored = std::fs::read(paths.share_dir().join(CERT_FILE)).unwrap();
        assert_eq!(first.pin().0, <[u8; 32]>::from(Sha256::digest(&stored)));
        second.server_config().expect("a reloaded identity serves");
    }

    /// Whatever has happened to the files, a damaged identity is reported and
    /// left exactly as found — the admin may be able to restore it, and a
    /// replacement would break every link already out there.
    #[tokio::test]
    async fn a_damaged_identity_is_an_error_and_left_alone() {
        let donor = Scratch::new("identity-donor");
        let other_key = Identity::load_or_create(&donor.paths).await.unwrap().key_der;

        // Which file, and what it now holds — `None` for gone.
        let damages: [(&str, &str, Option<&[u8]>); 6] = [
            ("garbage key", KEY_FILE, Some(b"not a key")),
            ("empty key", KEY_FILE, Some(b"")),
            ("another machine's key", KEY_FILE, Some(&other_key)),
            ("garbage certificate", CERT_FILE, Some(b"not a cert")),
            ("certificate deleted", CERT_FILE, None),
            ("key deleted", KEY_FILE, None),
        ];

        for (i, (what, file, now)) in damages.into_iter().enumerate() {
            let temp = Scratch::new(&format!("identity-damaged-{i}"));
            let paths = &temp.paths;
            Identity::load_or_create(paths).await.unwrap();
            let damaged = paths.share_dir().join(file);
            match now {
                Some(bytes) => std::fs::write(&damaged, bytes).unwrap(),
                None => std::fs::remove_file(&damaged).unwrap(),
            }
            let before = files(paths);

            let result = Identity::load_or_create(paths).await;

            assert!(result.is_err(), "{what}: should be refused");
            assert_eq!(
                files(paths),
                before,
                "{what}: the files must be left untouched"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_key_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let temp = Scratch::new("identity-mode");
        Identity::load_or_create(&temp.paths).await.unwrap();

        let mode = std::fs::metadata(temp.paths.share_dir().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The first share on a machine can ask for the identity from two places
    /// at once. Both must get the same one, not an error and not a pair of
    /// files that belong to different keys.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn simultaneous_first_calls_agree_on_one_identity() {
        let temp = Scratch::new("identity-race");
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let paths = temp.paths.clone();
                tokio::spawn(async move { Identity::load_or_create(&paths).await })
            })
            .collect();

        let mut pins = Vec::new();
        for call in calls {
            pins.push(
                call.await
                    .unwrap()
                    .expect("every caller gets the identity")
                    .pin(),
            );
        }

        let saved = Identity::load_or_create(&temp.paths).await.unwrap().pin();
        assert!(
            pins.iter().all(|pin| *pin == saved),
            "all callers must see the saved identity"
        );
    }
}
