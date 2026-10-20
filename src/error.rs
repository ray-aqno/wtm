//! Error type for `wtm`.
//!
//! P10 Rule 7: every fallible call returns a `Result`; this is the one error
//! type the whole crate propagates with `?`, so no call site silently drops a
//! failure.

use std::fmt;

/// All the ways a `wtm` operation can fail.
#[derive(Debug)]
pub enum Error {
    /// An I/O error from the filesystem or a spawned process.
    Io(std::io::Error),
    /// A git invocation exited non-zero; carries stderr for context.
    Git(String),
    /// A git invocation was killed after exceeding its timeout.
    Timeout(String),
    /// User input failed validation (branch name, path, version).
    Invalid(String),
    /// The requested worktree does not exist / is not known.
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Git(s) => write!(f, "git failed: {s}"),
            Error::Timeout(s) => write!(f, "git timed out: {s}"),
            Error::Invalid(s) => write!(f, "invalid input: {s}"),
            Error::NotFound(s) => write!(f, "not found: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
