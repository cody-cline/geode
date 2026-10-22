//! What the host is told before it starts, and the only way to tell it.
//!
//! All of it comes from the environment, because that is what a container
//! passes and what a compose file is written in. There are no command-line
//! options on purpose: two ways to say where the data lives is one way too
//! many, and [`Paths`](crate::paths::Paths) already reads `GEODE_HOME` for the
//! whole library.
//!
//! ```text
//! GEODE_TOKEN         required. The secret every request must present.
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
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        Self::read(|name| std::env::var(name).ok())
    }

    /// [`Settings::from_env`] against any source of variables — which is how
    /// this is tested, since a test that set real environment variables would
    /// set them for every other test running beside it.
    pub fn read(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let token = match get("GEODE_TOKEN").map(|raw| raw.trim().to_string()) {
            Some(raw) if raw.len() >= MIN_LENGTH => Token::new(&raw),
            Some(raw) if raw.is_empty() => return Err(no_token()),
            Some(raw) => {
                return Err(Error::msg(format!(
                    "GEODE_TOKEN is only {} characters long. Use at least {MIN_LENGTH}, or anyone who finds this port can guess it — `openssl rand -hex 32` makes a good one.",
                    raw.len()
                )))
            }
            None => return Err(no_token()),
        };

        let listen = get("GEODE_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let listen = listen.trim().parse().map_err(|_| {
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
        })
    }
}

/// Said the same way whether the variable was missing or empty, because the
/// mistake and the fix are the same.
fn no_token() -> Error {
    Error::msg(
        "GEODE_TOKEN is not set. The host will not start without one, because a management API with no token is an open door to every world on this machine — generate one with `openssl rand -hex 32`.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
