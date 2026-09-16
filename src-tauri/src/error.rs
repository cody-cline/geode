use serde::{Serialize, Serializer};

/// Every fallible operation in the backend funnels through this type. It is
/// serialized to a plain string for the frontend, so the `Display` text is
/// user-facing — keep it readable, not a debug dump.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not parse data from {context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("{0}")]
    Msg(String),

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("no account is signed in — add an account before launching")]
    NoAccount,

    #[error("sign-in expired, please sign in again")]
    AuthExpired,

    #[error("this Microsoft account does not own Minecraft: Java Edition")]
    NoEntitlement,

    #[error("instance '{0}' was not found")]
    NoSuchInstance(String),

    #[error("no usable Java runtime found for Java {0}")]
    NoJava(u32),

    /// A shared server turned a request away. Its own variant because pressing
    /// Play has to tell this apart from a server that is simply not there: one
    /// is fixed by opening a new link, the other by waiting.
    #[error("the server did not accept this password — it may have been replaced; ask the admin for the current link")]
    PasswordRefused,

    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }

    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Error::Json {
            context: context.into(),
            source,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error::Json {
            context: "response".into(),
            source,
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Adds `.ctx("what I was doing")` to any Result, so failures deep in the
/// download pipeline still say which file or step they came from.
pub trait Context<T> {
    fn ctx(self, what: impl std::fmt::Display) -> Result<T>;
}

impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
    fn ctx(self, what: impl std::fmt::Display) -> Result<T> {
        self.map_err(|e| Error::Msg(format!("{what}: {e}")))
    }
}
