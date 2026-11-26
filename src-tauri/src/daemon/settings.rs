//! What the host is told before it starts, and the only way to tell it.
//!
//! All of it comes from the environment, because that is what a container
//! passes and what a compose file is written in. There are no command-line
//! options on purpose: two ways to say where the data lives is one way too
//! many, and [`Paths`](crate::paths::Paths) already reads `GEODE_HOME` for the
//! whole library.
//!
//! ```text
//! GEODE_TOKEN_FILE    a file holding the secret every request must present.
//! GEODE_TOKEN         that secret itself. One of these two is required, and
//!                     setting both is refused — see [`read_token`].
//! GEODE_LISTEN        address:port for the management API. 127.0.0.1:8765.
//! GEODE_MAX_HEAP_MB   ceiling on one server's heap. Unset means no ceiling.
//! GEODE_HOME          where everything lives. Read by `paths.rs`, not here.
//! ```

use std::net::SocketAddr;

use crate::error::{Error, Result};

use super::token::{Token, MIN_LENGTH};

/// Where the management API listens when nothing says otherwise.
///
/// Loopback, not `0.0.0.0`: this listener has no TLS of its own, and a default
/// that reaches the network is a default that puts somebody's worlds on the
/// internet because they did not read far enough. The container image sets
/// `GEODE_LISTEN=0.0.0.0:8765` for itself, where the only way in is a port the
/// person running it chose to publish.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8765";

#[derive(Debug)]
pub struct Settings {
    pub token: Token,
    pub listen: SocketAddr,
    /// The most heap any one server may be started with, if the admin set a
    /// ceiling. `None` means a server is started with whatever it asks for,
    /// which is what the desktop app does.
    pub max_heap_mb: Option<u32>,
    /// What was worth saying about how this host was configured, but not worth
    /// refusing to start over. [`crate::daemon::run`] says each one through the
    /// sink, so it reaches a container's log rather than only a terminal.
    ///
    /// Said rather than returned as an error for one reason each, written where
    /// the warning is made. A refusal here is a host that will not come up, so
    /// it is kept for the things that are certainly wrong.
    pub warnings: Vec<String>,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        Self::read(|name| std::env::var(name).ok())
    }

    /// [`Settings::from_env`] against any source of variables — which is how
    /// this is tested, since a test that set real environment variables would
    /// set them for every other test running beside it.
    ///
    /// The token file named by `GEODE_TOKEN_FILE` is read from the real
    /// filesystem whichever source the variables came from: a path is only
    /// worth having instead of a secret if it is a path to something, and its
    /// permissions are half of what is being decided.
    pub fn read(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut warnings = Vec::new();
        let token = read_token(&get, &mut warnings)?;

        let listen = get("GEODE_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let listen = listen.trim().parse().map_err(|_| {
            // "over IPv6 as well" is a promise, and `serve::bind` is what keeps
            // it: it clears `IPV6_V6ONLY`, which Windows turns on, so `[::]`
            // there really does answer an IPv4 client. Without that this line
            // is advice to come up unreachable — change one and read the other.
            Error::msg(format!(
                "GEODE_LISTEN is {listen:?}, which is not an address and a port. Write it as {DEFAULT_LISTEN}, or [::]:8765 to listen over IPv6 as well."
            ))
        })?;

        let max_heap_mb = match get("GEODE_MAX_HEAP_MB").map(|raw| raw.trim().to_string()) {
            None => None,
            Some(raw) if raw.is_empty() => None,
            Some(raw) => Some(raw.parse().map_err(|_| {
                Error::msg(format!(
                    "GEODE_MAX_HEAP_MB is {raw:?}, which is not a number of megabytes."
                ))
            })?),
        };

        Ok(Self {
            token,
            listen,
            max_heap_mb,
            warnings,
        })
    }
}

/// Where the token comes from, and the one thing read here that is a secret
/// rather than a setting.
///
/// `GEODE_TOKEN_FILE` exists because `GEODE_TOKEN` cannot be made safe on
/// Linux. [`crate::childenv`] keeps Geode's own variables out of every child it
/// spawns, so a mod can no longer read the token out of its *own* environment —
/// but a Minecraft server runs as the same user as the host, and
/// `/proc/<geoded-pid>/environ` still holds the block `geoded` was exec'd with.
/// One `Files.readString` there and a mod has the only credential this API has.
/// A file closes that: what is in the environment is a path, and a path is not
/// a credential — whoever can read the file could have read the token anyway.
///
/// `std::env::remove_var` is deliberately not called after reading, and would
/// not be a substitute for any of this. `/proc/<pid>/environ` is the block the
/// process was *exec'd* with and does not change when the process edits its own
/// environment, so unsetting `GEODE_TOKEN` here would leave the hole exactly
/// where it was while reading as though it had been closed. It would hide the
/// variable only from code inside this process, which is not where the problem
/// is, and it would do it by mutating a global the tokio runtime's threads are
/// already reading.
///
/// **Both set is refused.** Preferring the file would also have been
/// defensible, and this is the louder of the two: two variables are two claims
/// about one secret, and whichever won, the other is a token somebody believes
/// is in use. A token rotated in the file while the old one is still in the
/// compose file would then keep authenticating the old one, or stop
/// authenticating the new one, and the admin finds out from a request rather
/// than from the host. Nothing already running breaks — `GEODE_TOKEN` alone is
/// still read exactly as it was, and a host only sees this by being given the
/// new variable and keeping the old.
fn read_token(get: &impl Fn(&str) -> Option<String>, warnings: &mut Vec<String>) -> Result<Token> {
    // Empty is unset, for both of them: that is what `GEODE_TOKEN` already did
    // when it was the only way, and `${GEODE_TOKEN_FILE:-}` in a compose file
    // arrives as an empty string rather than as nothing at all.
    let set = |name| {
        get(name)
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
    };

    let (raw, where_from) = match (set("GEODE_TOKEN"), set("GEODE_TOKEN_FILE")) {
        (Some(_), Some(_)) => return Err(two_tokens()),
        (Some(raw), None) => (raw, "in GEODE_TOKEN".to_string()),
        (None, Some(path)) => (read_token_file(&path, warnings)?, format!("in {path}")),
        (None, None) => return Err(no_token()),
    };

    if raw.len() < MIN_LENGTH {
        return Err(Error::msg(format!(
            "The token {where_from} is only {} characters long. Use at least {MIN_LENGTH}, or anyone who finds this port can guess it — `openssl rand -hex 32` makes a good one.",
            raw.len()
        )));
    }
    Ok(Token::new(&raw))
}

/// The whole of the file is the token, once its surrounding whitespace is off.
fn read_token_file(path: &str, warnings: &mut Vec<String>) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        Error::msg(format!(
            "GEODE_TOKEN_FILE is {path:?}, which could not be read: {e}. Point it at a file the user the host runs as can open — inside a container that is the path the secret is mounted at, not the path to it on the machine outside."
        ))
    })?;

    // Trimmed, and trimmed at the ends only. `openssl rand -hex 32 > token`
    // leaves a newline and so does every editor that has ever saved a file, so
    // a token file that had to be exact would be wrong on the first try almost
    // every time — and the failure is a 401 against a token that looks right in
    // both places. The middle of a token is the token, so nothing there moves.
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Err(Error::msg(format!(
            "GEODE_TOKEN_FILE is {path:?}, and there is no token in that file. Write one into it and nothing else — `openssl rand -hex 32 > {path}` — since the whole file is taken as the token once the whitespace around it is trimmed off."
        )));
    }

    warnings.extend(who_else_can_read(path));
    Ok(raw)
}

/// Whether the token file is only the host's to read, said as a warning rather
/// than a refusal.
///
/// A token file every user on the box can read is no better than the
/// environment variable it replaced, so this has to be said. It is not a
/// refusal because the mode on that file is often not the admin's to choose: a
/// secret mounted into a container arrives with whatever mode it was given
/// outside, and a host that will not start over a mode nobody there can change
/// is a host that cannot use the one mechanism this was added for — leaving
/// `GEODE_TOKEN`, which is strictly worse, as the only way to run it.
///
/// Any bit for group or other counts, not only the read ones: a token file
/// somebody else can rewrite is a token somebody else can choose.
#[cfg(unix)]
fn who_else_can_read(path: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let mode = std::fs::metadata(path).ok()?.mode() & 0o777;
    (mode & 0o077 != 0).then(|| {
        // `chmod 600` without the path on purpose: a mounted secret is
        // read-only at the path named here, and the file to change is the one
        // outside the container. Naming this path would be advice that fails.
        format!(
            "{path} is mode {mode:04o}, so other users on this machine can read or change the \
             host's token — `chmod 600` it where it really lives, or a token file is no safer \
             than GEODE_TOKEN was"
        )
    })
}

/// Windows has no mode bits, and its ACLs are a bigger question than one line
/// here could answer honestly. Nothing is claimed about the file there.
#[cfg(not(unix))]
fn who_else_can_read(_path: &str) -> Option<String> {
    None
}

/// Said the same way whether the variable was missing or empty, because the
/// mistake and the fix are the same.
fn no_token() -> Error {
    Error::msg(
        "Neither GEODE_TOKEN_FILE nor GEODE_TOKEN is set. The host will not start without one, because a management API with no token is an open door to every world on this machine — generate a token with `openssl rand -hex 32`, write it to a file, and point GEODE_TOKEN_FILE at that.",
    )
}

/// Two variables, one secret. See [`read_token`] for why this is refused rather
/// than resolved.
fn two_tokens() -> Error {
    Error::msg(
        "Unset one of GEODE_TOKEN and GEODE_TOKEN_FILE. Both are set, which is two claims about one secret, so whichever this host chose the other would still be a token somebody believes is in use.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// A real token file, in a scratch directory that goes when the test does.
    ///
    /// Not faked, because `GEODE_TOKEN_FILE` is a path and half of what is
    /// being decided is what the file on the end of it is like. Written 0600
    /// unless a test says otherwise: `fs::write` leaves whatever the umask
    /// allows, which on most machines is 0644 — the very thing
    /// [`a_token_file_other_users_can_read_is_said_out_loud_and_still_starts`]
    /// is about, and it would otherwise turn up in every test here.
    fn token_file(scratch: &Scratch, contents: &str) -> String {
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        let path = scratch.paths.root.join("token");
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path.to_string_lossy().into_owned()
    }

    /// A reader over a fixed list, standing in for the environment.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    const GOOD: &str = "f3a9c1d7e5b204860000";

    /// The whole reason this is not optional: no token, no listener.
    #[test]
    fn it_will_not_start_without_a_token() {
        for env in [vec![], vec![("GEODE_TOKEN", "")], vec![("GEODE_TOKEN", "   ")]] {
            let refused = Settings::read(vars(&env)).expect_err("should refuse to start");
            assert!(
                refused.to_string().contains("GEODE_TOKEN"),
                "the message must name what to set: {refused}"
            );
        }
    }

    /// A token short enough to guess is an open door with a latch on it.
    #[test]
    fn it_will_not_start_with_a_token_short_enough_to_guess() {
        let refused = Settings::read(vars(&[("GEODE_TOKEN", "changeme")]))
            .expect_err("eight characters is not a token");
        assert!(refused.to_string().contains("at least 16"), "{refused}");
    }

    #[test]
    fn a_token_is_taken_as_pasted_once_its_surroundings_are_trimmed() {
        let settings = Settings::read(vars(&[("GEODE_TOKEN", "  f3a9c1d7e5b204860000\n")])).unwrap();
        assert!(settings.token.matches(GOOD));
    }

    /// The point of the whole variable: the token authenticates, and the only
    /// thing that was ever in an environment block is a path.
    ///
    /// Written with the trailing newline `openssl rand -hex 32 > token` leaves,
    /// because that is what the file will really look like and a host that
    /// refused it would be a host nobody could configure first try.
    #[test]
    fn a_token_can_come_from_a_file_rather_than_the_environment() {
        let scratch = Scratch::new("token-file");
        let path = token_file(&scratch, &format!("{GOOD}\n"));

        let settings = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)])).unwrap();
        assert!(
            settings.token.matches(GOOD),
            "the token in the file is not the token the host will accept"
        );
        assert!(
            settings.warnings.is_empty(),
            "a file only its owner can read has nothing to warn about: {:?}",
            settings.warnings
        );
    }

    /// Two variables are two claims about one secret, and the host says so
    /// rather than picking. See `read_token` for why this is the refusal and
    /// not a precedence rule.
    #[test]
    fn both_ways_of_saying_the_token_is_refused_rather_than_one_quietly_winning() {
        let scratch = Scratch::new("token-both");
        let path = token_file(&scratch, GOOD);

        let refused = Settings::read(vars(&[
            ("GEODE_TOKEN", GOOD),
            ("GEODE_TOKEN_FILE", &path),
        ]))
        .expect_err("two claims about one secret must not resolve silently");
        let said = refused.to_string();
        assert!(
            said.contains("GEODE_TOKEN") && said.contains("GEODE_TOKEN_FILE"),
            "the message must name both, since either could be the one to remove: {said}"
        );
    }

    /// `${GEODE_TOKEN_FILE:-}` in a compose file arrives as an empty string,
    /// not as nothing, so an unused variable must not read as a second claim
    /// on the secret and refuse a host that is configured correctly.
    #[test]
    fn an_empty_token_file_variable_is_the_same_as_an_unset_one() {
        for blank in ["", "   "] {
            let settings = Settings::read(vars(&[
                ("GEODE_TOKEN", GOOD),
                ("GEODE_TOKEN_FILE", blank),
            ]))
            .unwrap_or_else(|e| panic!("GEODE_TOKEN_FILE={blank:?} is not a token file: {e}"));
            assert!(settings.token.matches(GOOD));
        }
    }

    /// A path to nothing is the mistake a container makes — the path outside
    /// the container rather than the one the secret is mounted at — so the
    /// message has to name the path it actually tried.
    #[test]
    fn a_token_file_that_is_not_there_is_refused_by_name() {
        let scratch = Scratch::new("token-missing");
        let path = scratch.paths.root.join("never-written");
        let path = path.to_string_lossy().into_owned();

        let refused = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)]))
            .expect_err("there is no file there");
        let said = refused.to_string();
        assert!(
            said.contains("GEODE_TOKEN_FILE") && said.contains("never-written"),
            "the message must name the variable and the path it tried: {said}"
        );
    }

    /// `touch token` and forgetting the rest must not start a host whose token
    /// is the empty string — which `Token::new("")` would happily accept, and
    /// which every request would then present by accident.
    #[test]
    fn a_token_file_with_no_token_in_it_is_refused() {
        let scratch = Scratch::new("token-blank");
        for contents in ["", "\n", "   \n\t\n"] {
            let path = token_file(&scratch, contents);
            let refused = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)]))
                .expect_err("a file holding only whitespace is not a token");
            let said = refused.to_string();
            assert!(
                said.contains("GEODE_TOKEN_FILE") && said.contains(&path),
                "the message must name the empty file: {said}"
            );
        }
    }

    /// A token too short to be worth having is refused whichever way it
    /// arrived, and the message names the file so there is somewhere to go.
    #[test]
    fn a_short_token_in_a_file_is_refused_the_same_way_as_a_short_one_in_the_environment() {
        let scratch = Scratch::new("token-short");
        let path = token_file(&scratch, "changeme\n");

        let refused = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)]))
            .expect_err("eight characters is not a token wherever it was written");
        let said = refused.to_string();
        assert!(said.contains("at least 16"), "{said}");
        assert!(
            said.contains(&path),
            "the message must say which file to fix: {said}"
        );
    }

    /// A token file every user on the box can read is no better than the
    /// variable it replaced, and saying nothing would leave somebody believing
    /// they had closed the hole.
    ///
    /// Said rather than refused on purpose — the mode is often not the admin's
    /// to choose inside a container, and a host that will not start over it is
    /// a host that has to go back to `GEODE_TOKEN`. So this also pins that it
    /// *starts*: a refusal here would be a regression, not a hardening.
    #[cfg(unix)]
    #[test]
    fn a_token_file_other_users_can_read_is_said_out_loud_and_still_starts() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("token-mode");
        let path = token_file(&scratch, GOOD);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let settings = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)]))
            .expect("a mode nobody can change must not stop the host starting");
        assert!(settings.token.matches(GOOD));
        let said = settings.warnings.join(" | ");
        assert!(
            said.contains(&path) && said.contains("chmod 600"),
            "the warning must name the file and what to do about it: {said:?}"
        );

        // And the same file, kept to itself, says nothing — or the warning is
        // printed on every start of every host and stops being read.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let quiet = Settings::read(vars(&[("GEODE_TOKEN_FILE", &path)])).unwrap();
        assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
    }

    /// The default must not reach the network: this listener has no TLS.
    #[test]
    fn it_listens_on_loopback_unless_told_otherwise() {
        let settings = Settings::read(vars(&[("GEODE_TOKEN", GOOD)])).unwrap();
        assert_eq!(settings.listen.to_string(), DEFAULT_LISTEN);
        assert!(settings.listen.ip().is_loopback());

        let container =
            Settings::read(vars(&[("GEODE_TOKEN", GOOD), ("GEODE_LISTEN", "0.0.0.0:8765")]))
                .unwrap();
        assert_eq!(container.listen.to_string(), "0.0.0.0:8765");
    }

    /// Mistyping the address must not quietly land on the default, which would
    /// look like the host ignoring what it was told.
    #[test]
    fn an_address_that_is_not_one_is_refused_rather_than_replaced() {
        for bad in ["8765", "0.0.0.0", "not an address", "0.0.0.0:not-a-port"] {
            let refused =
                Settings::read(vars(&[("GEODE_TOKEN", GOOD), ("GEODE_LISTEN", bad)]))
                    .unwrap_err();
            assert!(refused.to_string().contains("GEODE_LISTEN"), "{refused}");
        }
    }

    /// What the ceiling then does to a server is `server::run::start`'s, and
    /// tested there. This is only whether it was heard: unset must mean no
    /// ceiling rather than a ceiling of nothing, which would start every
    /// server with the smallest heap a JVM accepts.
    #[test]
    fn a_heap_ceiling_is_read_and_its_absence_means_no_ceiling() {
        let capped = Settings::read(vars(&[
            ("GEODE_TOKEN", GOOD),
            ("GEODE_MAX_HEAP_MB", "8192"),
        ]))
        .unwrap();
        assert_eq!(capped.max_heap_mb, Some(8192));

        for env in [
            vec![("GEODE_TOKEN", GOOD)],
            vec![("GEODE_TOKEN", GOOD), ("GEODE_MAX_HEAP_MB", "")],
        ] {
            assert_eq!(Settings::read(vars(&env)).unwrap().max_heap_mb, None);
        }
    }

    #[test]
    fn a_heap_ceiling_that_is_not_a_number_is_refused() {
        let refused = Settings::read(vars(&[
            ("GEODE_TOKEN", GOOD),
            ("GEODE_MAX_HEAP_MB", "8 gigabytes"),
        ]))
        .expect_err("that is not a number of megabytes");
        assert!(refused.to_string().contains("GEODE_MAX_HEAP_MB"), "{refused}");
    }
}
