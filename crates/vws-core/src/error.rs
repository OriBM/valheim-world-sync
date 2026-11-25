use std::path::Path;

/// Every failure the core reports. The UI turns these into plain-language messages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A compare-and-swap write lost: the remote file changed since it was read (I1).
    #[error("the shared state changed since it was read")]
    Conflict,
    /// The token was refused (HTTP 401).
    #[error("GitHub refused the access token; the setup code no longer works")]
    Unauthorized,
    #[error("not found on GitHub: {0}")]
    NotFound(String),
    #[error("GitHub rate limit reached (retry after {retry_after:?} s)")]
    RateLimited { retry_after: Option<u64> },
    #[error("network problem: {0}")]
    Network(String),
    #[error("GitHub answered HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("file problem at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid data: {0}")]
    Invalid(String),
    /// A hash or manifest check failed (I3, S8).
    #[error("verification failed: {0}")]
    Verify(String),
    /// The operation is not allowed in the current situation; the message says why.
    #[error("{0}")]
    Refused(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    /// Worth retrying later without user action: network trouble, rate limits,
    /// errors on GitHub's side.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::RateLimited { .. } => true,
            Error::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

/// Attach the path to a `std::io::Error`.
pub trait IoContext<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T> {
        self.map_err(|e| Error::io(path, e))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(format!("JSON: {e}"))
    }
}
