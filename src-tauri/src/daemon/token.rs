//! The one secret between a stranger and somebody's worlds.
//!
//! A share code ([`crate::share::code`]) is read aloud by players, so it is
//! ten Crockford characters with an alphabet chosen for ears. This is not that:
//! nobody types a host token from memory, it is pasted once into a compose
//! file, and it guards everything rather than one server's pack. So it is
//! whatever the admin generated — `openssl rand -hex 32`, as the compose file
//! says — and the only rules here are that there is one, that it is long
//! enough not to be guessed, and that checking it leaks nothing.
//!
//! What is kept is the digest, never the token. The daemon has no reason to
//! say its own token back to anybody, so after startup the plaintext is gone
//! from memory and cannot be printed by a stray debug line, an error message,
//! or a panic.

use sha2::{Digest, Sha256};

/// Short enough to guess is the same as no token at all.
///
/// Sixteen characters of anything random is past brute force over a network;
/// what this really turns away is `GEODE_TOKEN: changeme`.
pub const MIN_LENGTH: usize = 16;

/// A token, remembered only as what it hashes to.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

impl Token {
    pub fn new(raw: &str) -> Self {
        Self(Sha256::digest(raw.as_bytes()).into())
    }

    /// Whether `presented` is the token, without leaking how much of it was
    /// right — or how long the real one is.
    ///
    /// The same fold [`crate::share::code::Code::matches`] uses, over digests
    /// rather than the tokens themselves: both sides are then exactly 32 bytes
    /// however long what arrived was, so not even the length of the real token
    /// can be read off the time this takes.
    pub fn matches(&self, presented: &str) -> bool {
        let theirs: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        self.0
            .iter()
            .zip(theirs.iter())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
    }
}

/// Deliberately says nothing. A token that can be printed is a token that ends
/// up in a log.
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_matches_itself_and_nothing_else() {
        let token = Token::new("f3a9c1d7e5b20486f3a9c1d7e5b20486");
        assert!(token.matches("f3a9c1d7e5b20486f3a9c1d7e5b20486"));
        for wrong in [
            "",
            "f3a9c1d7e5b20486f3a9c1d7e5b20487", // one character out
            "f3a9c1d7e5b20486",                 // a prefix of the real one
            "f3a9c1d7e5b20486f3a9c1d7e5b20486 ", // pasted with a space
            "F3A9C1D7E5B20486F3A9C1D7E5B20486", // a token is not a share code
        ] {
            assert!(!token.matches(wrong), "{wrong:?} must not be accepted");
        }
    }

    /// Two different tokens must not collide, and the same one must compare
    /// equal however it was built — this is what the API's route guard rests
    /// on.
    #[test]
    fn tokens_are_told_apart_by_what_they_are() {
        assert_eq!(Token::new("the same token"), Token::new("the same token"));
        assert_ne!(Token::new("one token"), Token::new("another token"));
    }

    /// A token in a log is a token in a bug report. Asserted as the whole of
    /// what printing one produces, because a derived `Debug` would print the
    /// digest — which is not the token, but is enough to check guesses
    /// against offline.
    #[test]
    fn printing_a_token_says_nothing_about_it() {
        assert_eq!(
            format!("{:?}", Token::new("hunter2hunter2hunter2")),
            "Token(<redacted>)"
        );
    }
}
