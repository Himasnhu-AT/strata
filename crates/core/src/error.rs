//! The one error type of strata-core. Owned by the lead; add variants, never remove or rename.

use std::fmt;
use std::path::PathBuf;

/// Why a core operation failed. The `Display` text is shown to people (dialogs, toasts) and to
/// agents (`CommandError::failed`), so it says what happened and to which file.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The file does not exist.
    NotFound { path: PathBuf },
    /// The OS refused access.
    PermissionDenied { path: PathBuf },
    /// The file looks binary (NUL bytes or invalid UTF-8 in most of the first 64 KB).
    Binary { path: PathBuf },
    /// Any other I/O error (`message` is the OS text).
    Io { path: PathBuf, message: String },
    /// A compressed file would not fit the space allowed for its decompressed copy.
    TooLarge { path: PathBuf, limit: u64 },
    /// A `.gz` stream is corrupt or truncated.
    Corrupt { path: PathBuf, message: String },
    /// The cancel check returned true.
    Cancelled,
    /// A search query could not be compiled (bad regex).
    BadQuery { message: String },
    /// A filter text did not parse; `at` is the byte offset of the problem.
    BadFilter { at: usize, message: String },
    /// A time or duration text did not parse.
    BadTime { text: String },
    /// A line or file id that does not exist.
    OutOfRange { what: String },
    /// The feature is not built yet (stubs during M0).
    NotYet { what: &'static str },
}

impl Error {
    /// An [`Error::Io`] or one of its specific kinds, from an OS error on `path`.
    pub fn io(path: impl Into<PathBuf>, e: &std::io::Error) -> Error {
        let path = path.into();
        match e.kind() {
            std::io::ErrorKind::NotFound => Error::NotFound { path },
            std::io::ErrorKind::PermissionDenied => Error::PermissionDenied { path },
            _ => Error::Io { path, message: e.to_string() },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound { path } => write!(f, "{}: no such file", path.display()),
            Error::PermissionDenied { path } => write!(f, "{}: permission denied", path.display()),
            Error::Binary { path } => write!(f, "{}: looks like a binary file, not a log", path.display()),
            Error::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Error::TooLarge { path, limit } => write!(f, "{}: decompressed size is over the {limit}-byte limit", path.display()),
            Error::Corrupt { path, message } => write!(f, "{}: corrupt compressed data: {message}", path.display()),
            Error::Cancelled => f.write_str("cancelled"),
            Error::BadQuery { message } => write!(f, "bad search: {message}"),
            Error::BadFilter { at, message } => write!(f, "bad filter at {at}: {message}"),
            Error::BadTime { text } => write!(f, "not a time or duration: {text}"),
            Error::OutOfRange { what } => write!(f, "out of range: {what}"),
            Error::NotYet { what } => write!(f, "{what}: not yet"),
        }
    }
}

impl std::error::Error for Error {}
