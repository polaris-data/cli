use std::path::PathBuf;

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TickError {
    #[error("{0}")]
    DatasetUnavailable(String),
    #[error("{0}")]
    InvalidArgument(String),
    #[error("another sync is already running: {0}")]
    LockHeld(PathBuf),
    #[error("{message}")]
    Request {
        status: Option<StatusCode>,
        message: String,
        retryable: bool,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TickError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::DatasetUnavailable(_) => 2,
            Self::InvalidArgument(_)
            | Self::LockHeld(_)
            | Self::Request { .. }
            | Self::Other(_) => 1,
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Request { retryable, .. } => *retryable,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, TickError>;

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::TickError;

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(TickError::DatasetUnavailable("x".into()).exit_code(), 2);
        assert_eq!(TickError::InvalidArgument("x".into()).exit_code(), 1);
        assert_eq!(TickError::Other(anyhow!("boom")).exit_code(), 1);
    }
}
