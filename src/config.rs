use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow};
use directories::ProjectDirs;

use crate::auth::{CredentialStore, KeychainCredentialStore};
use crate::error::{Result, TickError};

const DEFAULT_BASE_URL: &str = "https://api.polaris.supply";
const DEFAULT_CONCURRENCY: usize = 4;
const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: Option<String>,
    pub api_key_source: Option<ApiKeySource>,
    pub root: PathBuf,
    pub concurrency: usize,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeySource {
    Environment,
    CredentialStore,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let store = KeychainCredentialStore::new()?;
        Self::from_reader_and_store(|key| env::var(key).ok(), &store)
    }

    pub fn from_reader<F>(mut reader: F) -> Result<Self>
    where
        F: FnMut(&str) -> Option<String>,
    {
        Self::from_reader_and_store(&mut reader, &NoopCredentialStore)
    }

    pub fn from_reader_and_store<F, S>(mut reader: F, store: &S) -> Result<Self>
    where
        F: FnMut(&str) -> Option<String>,
        S: CredentialStore,
    {
        let base_url = reader("POLARIS_BASE_URL")
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let env_api_key = reader("POLARIS_API_KEY")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let stored_api_key = store.get_api_key()?;
        let (api_key, api_key_source) = match (env_api_key, stored_api_key) {
            (Some(value), _) => (Some(value), Some(ApiKeySource::Environment)),
            (None, Some(value)) => (Some(value), Some(ApiKeySource::CredentialStore)),
            (None, None) => (None, None),
        };

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
            api_key_source,
            root,
            concurrency,
            timeout: Duration::from_secs(timeout_secs),
        })
    }
}

#[derive(Debug)]
struct NoopCredentialStore;

impl CredentialStore for NoopCredentialStore {
    fn get_api_key(&self) -> Result<Option<String>> {
        Ok(None)
    }

    fn set_api_key(&self, _api_key: &str) -> Result<()> {
        Ok(())
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

    use crate::auth::CredentialStore;
    use crate::error::Result;

    use super::{ApiKeySource, Config, default_root};

    #[derive(Debug)]
    struct FakeCredentialStore {
        api_key: Option<String>,
    }

    impl CredentialStore for FakeCredentialStore {
        fn get_api_key(&self) -> Result<Option<String>> {
            Ok(self.api_key.clone())
        }

        fn set_api_key(&self, _api_key: &str) -> Result<()> {
            Ok(())
        }
    }

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

    #[test]
    fn env_api_key_overrides_stored_key() {
        let values = HashMap::from([("POLARIS_API_KEY".to_string(), "env-key".to_string())]);
        let store = FakeCredentialStore {
            api_key: Some("stored-key".to_string()),
        };
        let config =
            Config::from_reader_and_store(|key| values.get(key).cloned(), &store).expect("config");
        assert_eq!(config.api_key.as_deref(), Some("env-key"));
        assert_eq!(config.api_key_source, Some(ApiKeySource::Environment));
    }

    #[test]
    fn stored_key_is_used_when_env_var_is_missing() {
        let values = HashMap::<String, String>::new();
        let store = FakeCredentialStore {
            api_key: Some("stored-key".to_string()),
        };
        let config =
            Config::from_reader_and_store(|key| values.get(key).cloned(), &store).expect("config");
        assert_eq!(config.api_key.as_deref(), Some("stored-key"));
        assert_eq!(config.api_key_source, Some(ApiKeySource::CredentialStore));
    }
}
