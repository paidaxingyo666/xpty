use thiserror::Error;

/// Error type for xpty operations.
#[derive(Debug, Error)]
pub enum Error {
    /// An I/O error from the operating system.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Writer has already been taken from this master PTY.
    #[error("writer already taken")]
    WriterAlreadyTaken,

    /// Command was not found in PATH.
    #[error("command not found: {0}")]
    CommandNotFound(String),

    /// Command exists but is not executable.
    #[error("not executable: {0}")]
    NotExecutable(String),

    /// The specified path is a directory, not an executable.
    #[error("path is a directory: {0}")]
    IsDirectory(String),

    /// Failed to resolve the executable path.
    #[error("{0}")]
    PathResolution(String),

    /// Argument is not valid UTF-8.
    #[error("argument is not valid UTF-8")]
    InvalidUtf8,

    /// A Windows HRESULT error.
    #[cfg(windows)]
    #[error("windows error: HRESULT {0:#010x}")]
    Hresult(i32),

    /// Other error with a descriptive message.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Create an `Error::Other` from any displayable value.
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }
}

impl From<filedescriptor::Error> for Error {
    fn from(err: filedescriptor::Error) -> Self {
        Error::Other(err.to_string())
    }
}

/// Result type alias for xpty operations.
pub type Result<T> = std::result::Result<T, Error>;
