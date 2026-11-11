use std::io;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Io(#[from] io::Error),
    #[error("http: {0}")]
    Http(#[from] hyper::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("server answered {code}")]
    Status { code: u16, retry_after: Option<Duration> },
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("unexpected response: {0}")]
    Protocol(String),
    #[error("the file changed on the server while downloading")]
    FileChanged,
    #[error("the link expired and could not be refreshed")]
    LinkExpired,
    #[error("disk: {0}")]
    Disk(io::Error),
    #[error("not enough free space: need {need} bytes, {free} available")]
    DiskFull { need: u64, free: u64 },
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Integrity { expected: String, actual: String },
    #[error("no usable network interface")]
    NoRoute,
    #[error("the server kept failing: {0}")]
    GaveUp(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Whether trying again later can reasonably succeed.
    pub fn transient(&self) -> bool {
        match self {
            Error::Io(_) | Error::Http(_) | Error::Timeout(_) | Error::Tls(_) => true,
            Error::Status { code, .. } => matches!(code, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 509),
            _ => false,
        }
    }

    /// The server is telling us to back off (too many connections or requests).
    pub fn refusal(&self) -> bool {
        matches!(self, Error::Status { code: 429 | 503 | 509, .. })
    }

    /// The link itself may have stopped being valid (signed URLs, sessions).
    pub fn maybe_expired(&self) -> bool {
        matches!(self, Error::Status { code: 401 | 403 | 404 | 410, .. })
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}
