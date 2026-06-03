use anyhow::{Context, anyhow};
use keyring::{Entry, Error as KeyringError};

use crate::error::{Result, TickError};

const PRIMARY_SERVICE_NAME: &str = "polaris";
const LEGACY_SERVICE_NAME: &str = "tick";
const ACCOUNT_NAME: &str = "polaris-api-key";

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
        match self.primary_entry.get_password() {
            Ok(api_key) => {
                let api_key = api_key.trim().to_string();
                if api_key.is_empty() {
                    self.read_legacy_api_key()
                } else {
                    Ok(Some(api_key))
                }
            }
            Err(KeyringError::NoEntry) => self.read_legacy_api_key(),
            Err(err) => Err(TickError::Other(anyhow!(
                "failed to read Polaris API key from OS credential store: {err}"
            ))),
        }
    }

    fn set_api_key(&self, api_key: &str) -> Result<()> {
        self.primary_entry
            .set_password(api_key)
            .context("failed to store Polaris API key in OS credential store")
            .map_err(TickError::Other)?;
        let _ = self.legacy_entry.set_password(api_key);
        Ok(())
    }
}

impl KeychainCredentialStore {
    fn read_legacy_api_key(&self) -> Result<Option<String>> {
        match self.legacy_entry.get_password() {
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
}
