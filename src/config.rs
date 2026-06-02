use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow};
use directories::ProjectDirs;

use crate::error::{Result, TickError};

const DEFAULT_BASE_URL: &str = "https://api.polaris.supply";
const DEFAULT_CONCURRENCY: usize = 4;
const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: Option<String>,
    pub root: PathBuf,
    pub concurrency: usize,
    pub timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_reader(|key| env::var(key).ok())
    }

    pub fn from_reader<F>(mut reader: F) -> Result<Self>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let base_url = reader("POLARIS_BASE_URL")
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let api_key = reader("POLARIS_API_KEY")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let root = match reader("TICK_ROOT") {
            Some(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => default_root()?,
        };

        let concurrency = parse_positive_usize(
            reader("TICK_CONCURRENCY"),
            DEFAULT_CONCURRENCY,
            "TICK_CONCURRENCY",
        )?;
        let timeout_secs = parse_positive_u64(
            reader("TICK_TIMEOUT_SECS"),
            DEFAULT_TIMEOUT_SECS,
            "TICK_TIMEOUT_SECS",
        )?;

        Ok(Self {
            base_url,
            api_key,
            root,
            concurrency,
            timeout: Duration::from_secs(timeout_secs),
        })
    }
}

pub fn default_root() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "tick")
        .ok_or_else(|| TickError::Other(anyhow!("unable to determine platform data directory")))?;
    Ok(dirs.data_local_dir().to_path_buf())
}

fn parse_positive_usize(raw: Option<String>, default: usize, name: &str) -> Result<usize> {
    match raw {
        None => Ok(default),
        Some(value) => {
            let parsed = value
                .trim()
                .parse::<usize>()
                .with_context(|| format!("failed to parse {name}"))
                .map_err(TickError::Other)?;
            if parsed == 0 {
                return Err(TickError::InvalidArgument(format!(
                    "{name} must be greater than zero"
                )));
            }
            Ok(parsed)
        }
    }
}

fn parse_positive_u64(raw: Option<String>, default: u64, name: &str) -> Result<u64> {
    match raw {
        None => Ok(default),
        Some(value) => {
            let parsed = value
                .trim()
                .parse::<u64>()
                .with_context(|| format!("failed to parse {name}"))
                .map_err(TickError::Other)?;
            if parsed == 0 {
                return Err(TickError::InvalidArgument(format!(
                    "{name} must be greater than zero"
                )));
            }
            Ok(parsed)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use directories::ProjectDirs;

    use super::{Config, default_root};

    #[test]
    fn default_root_matches_directories_crate() {
        let expected = ProjectDirs::from("", "", "tick")
            .expect("project dirs")
            .data_local_dir()
            .to_path_buf();
        assert_eq!(default_root().expect("default root"), expected);
    }

    #[test]
    fn root_override_is_respected() {
        let values = HashMap::from([("TICK_ROOT".to_string(), "/tmp/tick-root".to_string())]);
        let config = Config::from_reader(|key| values.get(key).cloned()).expect("config");
        assert_eq!(config.root, PathBuf::from("/tmp/tick-root"));
    }
}
