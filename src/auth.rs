use std::fs;
use std::path::PathBuf;

use anyhow::{Context, anyhow};
use directories::ProjectDirs;
use keyring::{Entry, Error as KeyringError};

use crate::error::{Result, TickError};

const PRIMARY_SERVICE_NAME: &str = "polaris";
const LEGACY_SERVICE_NAME: &str = "tick";
const ACCOUNT_NAME: &str = "polaris-api-key";
const PRIMARY_APP_NAME: &str = "polaris";
const LEGACY_APP_NAME: &str = "tick";

pub trait CredentialStore {
    fn get_api_key(&self) -> Result<Option<String>>;
    fn set_api_key(&self, api_key: &str) -> Result<()>;
}

#[derive(Debug)]
pub struct KeychainCredentialStore {
    primary_entry: Entry,
    legacy_entry: Entry,
}

impl KeychainCredentialStore {
    pub fn new() -> Result<Self> {
        let primary_entry = Entry::new(PRIMARY_SERVICE_NAME, ACCOUNT_NAME)
            .context("failed to initialize OS credential store")
            .map_err(TickError::Other)?;
        let legacy_entry = Entry::new(LEGACY_SERVICE_NAME, ACCOUNT_NAME)
            .context("failed to initialize OS credential store")
            .map_err(TickError::Other)?;
        Ok(Self {
            primary_entry,
            legacy_entry,
        })
    }
}

impl CredentialStore for KeychainCredentialStore {
    fn get_api_key(&self) -> Result<Option<String>> {
        let mut read_error: Option<TickError> = None;

        match self.read_entry(&self.primary_entry) {
            Ok(Some(api_key)) => return Ok(Some(api_key)),
            Ok(None) => {}
            Err(err) => read_error = Some(err),
        }

        match self.read_entry(&self.legacy_entry) {
            Ok(Some(api_key)) => return Ok(Some(api_key)),
            Ok(None) => {}
            Err(err) if read_error.is_none() => read_error = Some(err),
            Err(_) => {}
        }

        if let Some(api_key) = self.read_fallback_api_key(PRIMARY_APP_NAME)? {
            return Ok(Some(api_key));
        }
        if let Some(api_key) = self.read_fallback_api_key(LEGACY_APP_NAME)? {
            return Ok(Some(api_key));
        }

        if let Some(err) = read_error {
            return Err(err);
        }
        Ok(None)
    }

    fn set_api_key(&self, api_key: &str) -> Result<()> {
        let trimmed_api_key = api_key.trim();
        if trimmed_api_key.is_empty() {
            return Err(TickError::InvalidArgument("API key cannot be empty".into()));
        }

        let keychain_error = match self.primary_entry.set_password(trimmed_api_key) {
            Ok(()) => {
                let _ = self.legacy_entry.set_password(trimmed_api_key);
                if matches!(self.read_entry(&self.primary_entry), Ok(Some(stored)) if stored == trimmed_api_key)
                {
                    None
                } else {
                    Some(anyhow!(
                        "stored Polaris API key could not be read back from OS credential store"
                    ))
                }
            }
            Err(err) => {
                Some(anyhow!(err).context("failed to store Polaris API key in OS credential store"))
            }
        };

        self.write_fallback_api_key(PRIMARY_APP_NAME, trimmed_api_key)
            .or_else(|primary_err| {
                self.write_fallback_api_key(LEGACY_APP_NAME, trimmed_api_key)
                    .map_err(|legacy_err| {
                        TickError::Other(anyhow!(
                            "failed to persist Polaris API key in fallback file stores: {primary_err}; {legacy_err}"
                        ))
                    })
            })?;

        if let Some(err) = keychain_error {
            tracing::warn!("falling back to file-backed Polaris API key storage: {err}");
        }
        Ok(())
    }
}

impl KeychainCredentialStore {
    fn read_entry(&self, entry: &Entry) -> Result<Option<String>> {
        match entry.get_password() {
            Ok(api_key) => {
                let api_key = api_key.trim().to_string();
                if api_key.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(api_key))
                }
            }
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) => Err(TickError::Other(anyhow!(
                "failed to read Polaris API key from OS credential store: {err}"
            ))),
        }
    }

    fn read_fallback_api_key(&self, app_name: &str) -> Result<Option<String>> {
        let path = credential_fallback_path(app_name)?;
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(TickError::Other(anyhow!(err).context(format!(
                    "failed to read fallback credential file {}",
                    path.display()
                ))));
            }
        };

        let api_key = contents.trim().to_string();
        if api_key.is_empty() {
            Ok(None)
        } else {
            Ok(Some(api_key))
        }
    }

    fn write_fallback_api_key(&self, app_name: &str, api_key: &str) -> Result<()> {
        let path = credential_fallback_path(app_name)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                TickError::Other(anyhow!(err).context(format!(
                    "failed to create fallback credential directory {}",
                    parent.display()
                )))
            })?;
        }

        fs::write(&path, format!("{api_key}\n")).map_err(|err| {
            TickError::Other(anyhow!(err).context(format!(
                "failed to write fallback credential file {}",
                path.display()
            )))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, permissions).map_err(|err| {
                TickError::Other(anyhow!(err).context(format!(
                    "failed to set fallback credential file permissions {}",
                    path.display()
                )))
            })?;
        }

        Ok(())
    }
}

fn credential_fallback_path(app_name: &str) -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", app_name)
        .ok_or_else(|| TickError::Other(anyhow!("unable to determine platform data directory")))?;
    Ok(dirs.data_local_dir().join("account").join("api-key.txt"))
}
