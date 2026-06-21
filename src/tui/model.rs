use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::api::{DatasetAccess, DatasetAccessStatus};
use crate::syncer::SyncProgressEvent;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteDatasetEntry {
    pub venue: String,
    pub symbol: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: Option<String>,
    pub access: Option<DatasetAccess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    pub dataset: String,
}

impl RemoteDatasetEntry {
    pub(crate) fn access_badge(&self) -> String {
        let label = self
            .access
            .as_ref()
            .map(|access| access.status_label())
            .unwrap_or("unknown");
        format!("[{label}]")
    }

    pub fn access_summary(&self) -> String {
        self.access
            .as_ref()
            .map(DatasetAccess::summary_label)
            .unwrap_or_else(|| "unknown".into())
    }

    pub fn access_details(&self) -> String {
        match self.access.as_ref() {
            Some(DatasetAccess {
                status: DatasetAccessStatus::Open,
                ..
            }) => "public: all history publicly available".into(),
            Some(DatasetAccess {
                status: DatasetAccessStatus::Restricted,
                ..
            }) => "restricted: API key required for all history".into(),
            Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: Some(date),
            }) => format!("preview: public from {date} onward, API key required before that"),
            Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: None,
            }) => "preview: only recent history is public, older data requires an API key".into(),
            None => "unknown".into(),
        }
    }

    pub fn access_sort_order(&self) -> u8 {
        self.access
            .as_ref()
            .map(DatasetAccess::sort_order)
            .unwrap_or(u8::MAX)
    }

    pub(crate) fn matches_search(&self, search: &str) -> bool {
        let normalized = search.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return true;
        }

        let haystack = format!(
            "{} {} {} {}",
            self.dataset,
            self.source.as_deref().unwrap_or_default(),
            self.categories.join(" "),
            self.access
                .as_ref()
                .map(DatasetAccess::search_text)
                .unwrap_or_else(|| "unknown".into())
        )
        .to_ascii_lowercase();

        normalized.split_whitespace().all(|token| {
            if let Some(status) = token.strip_prefix("access:") {
                return self
                    .access
                    .as_ref()
                    .is_some_and(|access| access.status_label() == status);
            }
            haystack.contains(token)
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RemoteTuiSeed {
    pub venue: Option<String>,
    pub symbol: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LocalDatasetSummary {
    pub(crate) snapshot_count: usize,
    pub(crate) first_start: Option<DateTime<Utc>>,
    pub(crate) last_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DayState {
    Full,
    Partial,
    Empty,
    NoRemote,
}

#[derive(Debug, Clone)]
pub(crate) struct DayCoverage {
    pub(crate) date: NaiveDate,
    pub(crate) remote_keys: Vec<String>,
    pub(crate) local_keys: Vec<String>,
    pub(crate) missing_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileManagerTarget {
    File(PathBuf),
    Directory(PathBuf),
}

#[derive(Debug)]
pub(crate) struct DatasetView {
    pub(crate) dataset: RemoteDatasetEntry,
    pub(crate) days: Vec<DayCoverage>,
    pub(crate) selected_day: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AccountIdentity {
    pub(crate) user_id: String,
    pub(crate) display_name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) plan: Option<String>,
    pub(crate) wallet_address: Option<String>,
    pub(crate) avatar_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountLoginSession {
    pub(crate) user_code: String,
    pub(crate) login_url: String,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountView {
    pub(crate) api_key_present: bool,
    pub(crate) api_key_source_label: String,
    pub(crate) base_url: String,
    pub(crate) root: PathBuf,
    pub(crate) active_login: Option<AccountLoginSession>,
    pub(crate) identity: Option<AccountIdentity>,
}

#[derive(Debug, Default)]
pub(crate) struct ApiKeyPromptState {
    pub(crate) input: String,
    pub(crate) error_message: Option<String>,
    pub(crate) access_message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct BookmarkStore {
    pub(crate) bookmarks: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApiKeyRequirement {
    Restricted,
    Preview {
        public_cutoff_date: Option<NaiveDate>,
    },
    LegacyPreviewWindow,
}

impl ApiKeyRequirement {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Restricted => {
                "This dataset is restricted. A Polaris API key is required for all snapshot downloads."
                    .into()
            }
            Self::Preview {
                public_cutoff_date: Some(date),
            } => format!(
                "This dataset is preview-only before {date}. Older snapshot downloads require a Polaris API key."
            ),
            Self::Preview {
                public_cutoff_date: None,
            } => {
                "This dataset is preview-only. Older snapshot downloads require a Polaris API key."
                    .into()
            }
            Self::LegacyPreviewWindow => "Older than 7 days requires a Polaris API key.".into(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ActiveDaySync {
    pub(crate) dataset: String,
    pub(crate) date: NaiveDate,
    pub(crate) remote_total: usize,
    pub(crate) local_present: usize,
    pub(crate) downloaded: usize,
    pub(crate) failed: usize,
    pub(crate) download_bytes: u64,
    pub(crate) download_total_bytes: Option<u64>,
    pub(crate) deferred_update: Option<DaySyncUpdate>,
    pub(crate) receiver: UnboundedReceiver<DaySyncUpdate>,
}

#[derive(Debug)]
pub(crate) enum DaySyncUpdate {
    Started {
        total_bytes: Option<u64>,
    },
    Progress {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    },
    Downloaded {
        total_bytes: u64,
    },
    Failed,
    Finished {
        downloaded: usize,
        failed: usize,
    },
    Error(String),
}

#[derive(Debug)]
pub(crate) enum ViewMode {
    Splash,
    Browser,
    Account,
    Dataset(DatasetView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserCategory {
    AllDatasets,
    Bookmarks,
    Catalog(String),
}

impl BrowserCategory {
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::AllDatasets => "All",
            Self::Bookmarks => "Bookmarks",
            Self::Catalog(category) => category.as_str(),
        }
    }
}

impl From<SyncProgressEvent> for DaySyncUpdate {
    fn from(value: SyncProgressEvent) -> Self {
        match value {
            SyncProgressEvent::Started { total_bytes, .. } => {
                DaySyncUpdate::Started { total_bytes }
            }
            SyncProgressEvent::Progress {
                downloaded_bytes,
                total_bytes,
                ..
            } => DaySyncUpdate::Progress {
                downloaded_bytes,
                total_bytes,
            },
            SyncProgressEvent::Downloaded { total_bytes, .. } => {
                DaySyncUpdate::Downloaded { total_bytes }
            }
            SyncProgressEvent::Failed { .. } => DaySyncUpdate::Failed,
        }
    }
}

impl DatasetView {
    pub(crate) fn selected_coverage(&self) -> &DayCoverage {
        &self.days[self.selected_day]
    }

    pub(crate) fn move_selection(&mut self, delta_days: i64) {
        if self.days.is_empty() {
            return;
        }
        let next = self.selected_day as i64 + delta_days;
        let max = (self.days.len() - 1) as i64;
        self.selected_day = next.clamp(0, max) as usize;
    }
}

impl DayCoverage {
    pub(crate) fn state(&self) -> DayState {
        if self.remote_keys.is_empty() {
            DayState::NoRemote
        } else if self.missing_keys.is_empty() {
            DayState::Full
        } else if self.local_keys.is_empty() {
            DayState::Empty
        } else {
            DayState::Partial
        }
    }
}
