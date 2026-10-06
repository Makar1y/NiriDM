use std::path::PathBuf;

/// Central error type for the NiriDM core.
#[derive(Debug, thiserror::Error)]
pub enum NdError {
    #[error("failed to run `{0}`: {1}")]
    SpawnFailed(&'static str, #[source] std::io::Error),

    #[error("`{0}` exited with status {1})")]
    NonZeroExit(&'static str, String),

    #[error("failed to decode JSON from `{0}`: {1}")]
    Json(&'static str, #[source] serde_json::Error),

    #[error("niri reported an error: {0}")]
    Niri(String),

    #[error("failed to read `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no valid layout configured (searched {0} layout(s))")]
    NoMatchingLayout(usize),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, NdError>;
