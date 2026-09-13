//! Share codes: short enough to read aloud, useless to a guesser.
//!
//! A code is what somebody types or pastes into Geode to fetch a pack. It has
//! to survive being read out over voice chat, retyped from a phone screen, and
//! copied out of a Discord message that helpfully capitalised it — so the
//! alphabet is Crockford's Base32, which drops `I`, `L`, `O` and `U`, treats
//! `I`/`l` as `1` and `O` as `0`, and ignores case and hyphens.
//!
//! The code is generated here, on the machine sharing the pack, and never sent
//! anywhere. What a lookup service sees is [`Code::key`] — the first 80 bits of
//! `SHA-256("geode-share-v1" || code)` — so a dump of that service's storage
//! reveals no codes, and knowing a key does not let anyone work backwards to
//! one. The bytes of the pack never go near it either; it stores only where to
//! find them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Crockford's alphabet: no I, L, O or U, so nothing reads as something else.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Ten characters, fifty bits.
///
/// Enough that guessing is hopeless — a million tries a second would need
/// eighteen years for an even chance — and short enough to say out loud.
const LENGTH: usize = 10;

/// Domain separation, so a hash from this scheme can never collide with a hash
/// from another part of the app that happened to use SHA-256 too.
const DOMAIN: &[u8] = b"geode-share-v1";

/// What `GEODE` becomes once normalised — `O` is not in the alphabet and maps
/// to zero. Written out rather than computed so the surprise is visible.
const PREFIX: &str = "GE0DE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code(String);

impl Code {
    /// A fresh code, from the operating system's randomness.
    pub fn generate() -> Self {
        let mut bytes = [0u8; LENGTH];
        getrandom::fill(&mut bytes).expect("the OS always has randomness");
        Self(
            bytes
                .iter()
                .map(|b| ALPHABET[(*b % 32) as usize] as char)
                .collect(),
        )
    }

    /// Read a code somebody typed, however they typed it.
    ///
    /// Accepts the `GEODE-` prefix or not, any hyphens or spaces anywhere, any
    /// case, and the characters Crockford says stand in for others. Returns
    /// `None` when what is left is not a code at all, rather than guessing.
    pub fn parse(input: &str) -> Option<Self> {
        let mut out = String::with_capacity(LENGTH + PREFIX.len());
        for c in input.chars() {
            let upper = c.to_ascii_uppercase();
            let mapped = match upper {
                '-' | ' ' | '_' => continue,
                // Crockford's confusables, which is the whole point of the
                // alphabet: someone reading a code aloud says "oh", not "zero".
                'O' => '0',
                'I' | 'L' => '1',
                'U' => continue, // never emitted; treat as noise rather than fail
                other => other,
            };
            if !ALPHABET.contains(&(mapped as u8)) {
                // A stray letter means this is not a code — a URL, say.
                return None;
            }
            out.push(mapped);
            // Bounded so a pasted novel cannot make this work forever. The
            // allowance is the code plus the prefix people paste with it.
            if out.len() > LENGTH + PREFIX.len() {
                return None;
            }
        }
        // "GEODE" is a prefix people will paste, and every one of its letters
        // is also a valid code character — `O` normalises to `0`, giving
        // `GE0DE`. So it is stripped only when what remains is a whole code,
        // never when it might be the start of one.
        if out.len() == LENGTH + PREFIX.len() && out.starts_with(PREFIX) {
            out = out[PREFIX.len()..].to_string();
        }
        (out.len() == LENGTH).then_some(Self(out))
    }

    /// How it should be shown: `GEODE-XXXXX-XXXXX`.
    pub fn display(&self) -> String {
        format!("GEODE-{}-{}", &self.0[..5], &self.0[5..])
    }

    /// The bare characters, without decoration.
    pub fn raw(&self) -> &str {
        &self.0
    }

    /// What a lookup service is keyed by.
    ///
    /// Derived rather than the code itself, so the service never learns a code
    /// and a leak of its storage grants nothing. Sixteen characters of Base32
    /// is eighty bits, which is far beyond collision range for a service that
    /// will hold a few hundred entries.
    pub fn key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN);
        hasher.update(self.0.as_bytes());
        let digest = hasher.finalize();
        digest
            .iter()
            .take(16)
            .map(|b| ALPHABET[(*b % 32) as usize] as char)
            .collect()
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_code_round_trips_through_its_own_display() {
        for _ in 0..200 {
            let code = Code::generate();
            assert_eq!(code.raw().len(), LENGTH);
            assert_eq!(Code::parse(&code.display()).as_ref(), Some(&code));
        }
    }

    /// The whole reason for this alphabet. Somebody reading a code aloud says
    /// "oh" for zero and "eye" for one, and somebody retyping it from a phone
    /// gets the case wrong. All of that has to land on the same code.
    #[test]
    fn the_ways_people_mistype_a_code_all_land_on_the_same_one() {
        let canonical = Code::parse("GEODE-K3VB0-1NQ7Z").unwrap();
        for variant in [
            "geode-k3vb0-1nq7z",       // lower case
            "K3VB01NQ7Z",              // no prefix, no hyphens
            "k3vb0 1nq7z",             // spaces instead
            "GEODE-K3VBO-INQ7Z",       // O for zero, I for one
            "GEODE-k3vbO-lnq7z",       // l for one, mixed case
            "  GEODE-K3VB0-1NQ7Z  ",   // pasted with whitespace
        ] {
            assert_eq!(
                Code::parse(variant).as_ref(),
                Some(&canonical),
                "{variant:?} should read as the same code"
            );
        }
    }

    #[test]
    fn something_that_is_not_a_code_is_refused_rather_than_guessed_at() {
        for junk in [
            "",
            "GEODE-",
            "K3VB0",                        // too short
            "K3VB01NQ7ZEXTRA",              // too long
            "https://example.com/pack.zip", // a URL someone pasted
            "K3VB0-1NQ7!",                  // punctuation
        ] {
            assert!(Code::parse(junk).is_none(), "{junk:?} should not parse");
        }
    }

    /// The key must be stable — a code that resolved yesterday has to resolve
    /// today — and must not be reversible to the code.
    #[test]
    fn a_key_is_derived_stably_and_reveals_nothing() {
        let code = Code::parse("GEODE-K3VB0-1NQ7Z").unwrap();
        let key = code.key();
        assert_eq!(key, code.key(), "the same code always gives the same key");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| ALPHABET.contains(&(c as u8))));
        // The code must not be recoverable by looking at the key.
        assert!(!key.contains(code.raw()));

        let other = Code::parse("GEODE-K3VB0-1NQ70").unwrap();
        assert_ne!(code.key(), other.key(), "one character apart is a different key");
    }

    /// Codes are what stands between a stranger and somebody's pack, so the
    /// generator must not be quietly producing a handful of values.
    #[test]
    fn generated_codes_do_not_repeat() {
        let seen: std::collections::HashSet<String> = (0..2000)
            .map(|_| Code::generate().raw().to_string())
            .collect();
        assert_eq!(seen.len(), 2000, "a collision in 2000 draws means a broken generator");
    }
}
