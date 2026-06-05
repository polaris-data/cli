use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::block::Title;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError, unbounded_channel};

use crate::api::{DatasetAccess, DatasetAccessStatus, PolarisClient, SnapshotEntry};
use crate::auth::{CredentialStore, KeychainCredentialStore};
use crate::config::Config;
use crate::error::{Result, TickError};
use crate::layout::{
    Layout as AppLayout, LocalSnapshotEntry, infer_snapshot_date_from_key,
};
use crate::planner::{TimeWindow, build_sync_plan};
use crate::syncer::{SyncProgressEvent, acquire_sync_lock, execute_sync_with_progress};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteDatasetEntry {
    pub exchange: String,
    pub asset: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: Option<String>,
    pub access: Option<DatasetAccess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    pub dataset: String,
}

impl RemoteDatasetEntry {
    fn access_badge(&self) -> String {
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

    pub fn matches_search(&self, search: &str) -> bool {
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
    pub exchange: Option<String>,
    pub asset: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LocalDatasetSummary {
    snapshot_count: usize,
    first_start: Option<DateTime<Utc>>,
    last_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DayState {
    Full,
    Partial,
    Empty,
    NoRemote,
}

#[derive(Debug, Clone)]
struct DayCoverage {
    date: NaiveDate,
    remote_keys: Vec<String>,
    local_keys: Vec<String>,
    missing_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileManagerTarget {
    File(PathBuf),
    Directory(PathBuf),
}

#[derive(Debug)]
struct DatasetView {
    dataset: RemoteDatasetEntry,
    days: Vec<DayCoverage>,
    selected_day: usize,
}

#[derive(Debug, Default)]
struct ApiKeyPromptState {
    input: String,
    error_message: Option<String>,
    access_message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BookmarkStore {
    bookmarks: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApiKeyRequirement {
    Restricted,
    Preview {
        public_cutoff_date: Option<NaiveDate>,
    },
    LegacyPreviewWindow,
}

impl ApiKeyRequirement {
    fn message(&self) -> String {
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
            Self::LegacyPreviewWindow => {
                "Older than 7 days requires a Polaris API key.".into()
            }
        }
    }
}

#[derive(Debug)]
struct ActiveDaySync {
    dataset: String,
    date: NaiveDate,
    remote_total: usize,
    local_present: usize,
    downloaded: usize,
    failed: usize,
    download_bytes: u64,
    download_total_bytes: Option<u64>,
    deferred_update: Option<DaySyncUpdate>,
    receiver: UnboundedReceiver<DaySyncUpdate>,
}

#[derive(Debug)]
enum DaySyncUpdate {
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
enum ViewMode {
    Splash,
    Browser,
    Dataset(DatasetView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserCategory {
    AllDatasets,
    Bookmarks,
    Catalog(String),
}

impl BrowserCategory {
    fn label(&self) -> &str {
        match self {
            Self::AllDatasets => "All",
            Self::Bookmarks => "Bookmarks",
            Self::Catalog(category) => category.as_str(),
        }
    }
}

#[derive(Debug)]
struct RemoteListTui {
    datasets: Vec<RemoteDatasetEntry>,
    filtered_indices: Vec<usize>,
    selected: usize,
    search: String,
    categories: Vec<BrowserCategory>,
    selected_category: usize,
    bookmarks: BTreeSet<String>,
    session_priority_bookmarks: BTreeSet<String>,
    local_summaries: BTreeMap<String, LocalDatasetSummary>,
    local_keys: BTreeMap<String, Vec<String>>,
    root: PathBuf,
    concurrency: usize,
    status_message: Option<String>,
    active_sync: Option<ActiveDaySync>,
    spinner_tick: usize,
    mode: ViewMode,
    api_key_prompt: Option<ApiKeyPromptState>,
}

impl RemoteListTui {
    fn new(
        datasets: Vec<RemoteDatasetEntry>,
        local_snapshots: Vec<LocalSnapshotEntry>,
        root: PathBuf,
        concurrency: usize,
        seed: RemoteTuiSeed,
    ) -> Self {
        let mut search = seed.search.unwrap_or_default();
        if search.is_empty() {
            if let Some(exchange) = seed.exchange {
                search = exchange;
                if let Some(asset) = seed.asset {
                    search.push(':');
                    search.push_str(&asset);
                }
            }
        }

        let (bookmarks, bookmark_status) = match load_bookmarks(&root) {
            Ok(bookmarks) => (bookmarks, None),
            Err(err) => (
                BTreeSet::new(),
                Some(format!("warning: failed to load bookmarks: {err}")),
            ),
        };
        let local_summaries = summarize_local_snapshots(&local_snapshots);
        let local_keys = group_local_snapshot_keys(local_snapshots);
        let categories = browser_categories(&datasets);
        let mut app = Self {
            datasets,
            filtered_indices: Vec::new(),
            selected: 0,
            search,
            categories,
            selected_category: 0,
            bookmarks: bookmarks.clone(),
            session_priority_bookmarks: bookmarks,
            local_summaries,
            local_keys,
            root,
            concurrency,
            status_message: bookmark_status,
            active_sync: None,
            spinner_tick: 0,
            mode: ViewMode::Splash,
            api_key_prompt: None,
        };
        app.recompute_filter();
        app
    }

    fn recompute_filter(&mut self) {
        let selected_dataset = self
            .selected_dataset()
            .map(|dataset| dataset.dataset.clone());
        self.filtered_indices = self
            .datasets
            .iter()
            .enumerate()
            .filter_map(|(index, dataset)| {
                if self.matches_current_category(dataset) && dataset.matches_search(&self.search) {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        let datasets = &self.datasets;
        let bookmarks = &self.session_priority_bookmarks;
        self.filtered_indices.sort_by_key(|index| {
            let dataset = &datasets[*index];
            (!bookmarks.contains(dataset.dataset.as_str()), *index)
        });
        if self.filtered_indices.is_empty() {
            self.selected = 0;
        } else if let Some(selected_dataset) = selected_dataset
            && let Some(position) = self
                .filtered_indices
                .iter()
                .position(|index| self.datasets[*index].dataset == selected_dataset)
        {
            self.selected = position;
        } else if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len() - 1;
        }
    }

    fn selected_dataset(&self) -> Option<&RemoteDatasetEntry> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|index| self.datasets.get(*index))
    }

    fn selected_category(&self) -> &BrowserCategory {
        self.categories
            .get(self.selected_category)
            .expect("browser categories should always include defaults")
    }

    fn category_display_labels(&self) -> Vec<String> {
        self.categories
            .iter()
            .cycle()
            .skip(self.selected_category)
            .take(self.categories.len())
            .map(|category| category.label().to_ascii_lowercase())
            .collect()
    }

    fn matches_current_category(&self, dataset: &RemoteDatasetEntry) -> bool {
        match self.selected_category() {
            BrowserCategory::AllDatasets => true,
            BrowserCategory::Bookmarks => self.is_bookmarked(dataset.dataset.as_str()),
            BrowserCategory::Catalog(category) => dataset
                .categories
                .iter()
                .any(|candidate| candidate == category),
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.filtered_indices.len() {
            self.selected += 1;
        }
    }

    fn cycle_category(&mut self, delta: isize) {
        if self.categories.is_empty() {
            return;
        }

        let len = self.categories.len() as isize;
        let next = (self.selected_category as isize + delta).rem_euclid(len) as usize;
        if next != self.selected_category {
            self.selected_category = next;
            self.recompute_filter();
        }
    }

    fn current_dataset_id(&self) -> Option<&str> {
        match &self.mode {
            ViewMode::Dataset(view) => Some(view.dataset.dataset.as_str()),
            ViewMode::Browser | ViewMode::Splash => self
                .selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
        }
    }

    fn is_bookmarked(&self, dataset: &str) -> bool {
        self.bookmarks.contains(dataset)
    }

    fn toggle_current_bookmark(&mut self) -> Result<()> {
        let Some(dataset) = self.current_dataset_id().map(str::to_string) else {
            return Ok(());
        };

        let mut next = self.bookmarks.clone();
        let message = if next.insert(dataset.clone()) {
            format!("bookmarked {dataset}")
        } else {
            next.remove(&dataset);
            format!("removed bookmark for {dataset}")
        };

        save_bookmarks(&self.root, &next)?;
        self.bookmarks = next;
        self.recompute_filter();
        self.status_message = Some(message);
        Ok(())
    }

    fn dataset_view(&self) -> Option<&DatasetView> {
        match &self.mode {
            ViewMode::Dataset(view) => Some(view),
            ViewMode::Browser | ViewMode::Splash => None,
        }
    }

    async fn open_selected_dataset(&mut self, client: &PolarisClient) -> Result<()> {
        let Some(dataset) = self.selected_dataset().cloned() else {
            return Ok(());
        };

        let (remote_snapshots, _) = client
            .list_snapshots(
                &dataset.exchange,
                &dataset.asset,
                dataset.start,
                dataset.end,
            )
            .await?;
        let local_keys = self
            .local_keys
            .get(&dataset.dataset)
            .cloned()
            .unwrap_or_default();
        let days = build_day_coverages(
            remote_snapshots,
            &local_keys,
            dataset.start.date_naive(),
            dataset.end.date_naive(),
        );
        let selected_day = select_initial_day(&days);

        self.mode = ViewMode::Dataset(DatasetView {
            dataset,
            days,
            selected_day,
        });
        self.status_message = None;
        Ok(())
    }

    fn reload_local_state(&mut self) -> Result<()> {
        let layout = crate::layout::Layout::new(self.root.clone());
        let local_snapshots = layout.list_local_snapshots()?;
        self.local_summaries = summarize_local_snapshots(&local_snapshots);
        self.local_keys = group_local_snapshot_keys(local_snapshots);
        Ok(())
    }

    fn reveal_selected_day_snapshot(&mut self) -> Result<()> {
        let Some(view) = self.dataset_view() else {
            return Ok(());
        };

        let layout = AppLayout::new(self.root.clone());
        let day = view.selected_coverage();
        let keys = if day.local_keys.is_empty() {
            &day.remote_keys
        } else {
            &day.local_keys
        };
        let mut snapshot_paths = Vec::new();
        for key in keys {
            snapshot_paths.push(layout.data_path_for_key(key)?);
        }

        let data_root = layout.data_root();
        let fallback_path = snapshot_paths
            .first()
            .cloned()
            .unwrap_or_else(|| data_root.clone());
        match snapshot_reveal_target(&data_root, &snapshot_paths) {
            Some(FileManagerTarget::File(path)) => {
                open_in_file_manager(&FileManagerTarget::File(path))?;
                self.status_message = None;
            }
            Some(FileManagerTarget::Directory(path)) => {
                open_in_file_manager(&FileManagerTarget::Directory(path))?;
                self.status_message = None;
            }
            None => {
                self.status_message = Some(format!(
                    "snapshot folder not created yet: {}",
                    fallback_path.display()
                ));
            }
        }
        Ok(())
    }

    async fn sync_selected_day(&mut self, client: &PolarisClient) -> Result<()> {
        if self.active_sync.is_some() {
            self.status_message = Some("sync already in progress".into());
            return Ok(());
        }
        let Some(view) = self.dataset_view() else {
            return Ok(());
        };
        let dataset = view.dataset.clone();
        let selected_date = view.selected_coverage().date;
        let requested_range = TimeWindow {
            from: DateTime::<Utc>::from_naive_utc_and_offset(
                selected_date.and_hms_opt(0, 0, 0).expect("valid day start"),
                Utc,
            ),
            to: DateTime::<Utc>::from_naive_utc_and_offset(
                selected_date
                    .and_hms_opt(23, 59, 59)
                    .expect("valid day end"),
                Utc,
            ),
        };

        let config = Config {
            base_url: String::new(),
            api_key: None,
            api_key_source: None,
            root: self.root.clone(),
            concurrency: self.concurrency,
            timeout: Duration::from_secs(60),
        };
        let plan = build_sync_plan(
            client,
            &config,
            &dataset.exchange,
            &dataset.asset,
            requested_range,
        )
        .await?;
        let remote_total = plan.remote_total();
        let local_present = plan.present_total();
        let root = self.root.clone();
        let concurrency = self.concurrency;
        let client = client.clone();
        let (tx, rx) = unbounded_channel();
        let sync_plan = plan.clone();

        tokio::spawn(async move {
            let layout = crate::layout::Layout::new(root);
            let _guard = match acquire_sync_lock(&layout) {
                Ok(guard) => guard,
                Err(err) => {
                    let _ = tx.send(DaySyncUpdate::Error(err.to_string()));
                    return;
                }
            };

            let (progress_tx, mut progress_rx) = unbounded_channel();
            let mut sync_task = tokio::spawn({
                let client = client.clone();
                let sync_plan = sync_plan.clone();
                async move {
                    execute_sync_with_progress(&client, &sync_plan, concurrency, progress_tx).await
                }
            });

            let execution = loop {
                tokio::select! {
                    progress = progress_rx.recv() => {
                        match progress {
                            Some(progress) => {
                                let _ = tx.send(DaySyncUpdate::from(progress));
                            }
                            None => break match sync_task.await {
                                Ok(execution) => execution,
                                Err(err) => {
                                    let _ = tx.send(DaySyncUpdate::Error(err.to_string()));
                                    return;
                                }
                            },
                        }
                    }
                    result = &mut sync_task => {
                        break match result {
                            Ok(execution) => execution,
                            Err(err) => {
                                let _ = tx.send(DaySyncUpdate::Error(err.to_string()));
                                return;
                            }
                        };
                    }
                }
            };
            let _ = tx.send(DaySyncUpdate::Finished {
                downloaded: execution.downloaded_total(),
                failed: execution.failed_total(),
            });
        });

        self.active_sync = Some(ActiveDaySync {
            dataset: dataset.dataset.clone(),
            date: selected_date,
            remote_total,
            local_present,
            downloaded: 0,
            failed: 0,
            download_bytes: 0,
            download_total_bytes: None,
            deferred_update: None,
            receiver: rx,
        });
        self.status_message = Some(format!("syncing {}", selected_date));
        Ok(())
    }

    fn api_key_requirement_for_selected_day(
        &self,
        today: NaiveDate,
        has_api_key: bool,
    ) -> Option<ApiKeyRequirement> {
        let Some(view) = self.dataset_view() else {
            return None;
        };
        let day = view.selected_coverage();
        api_key_requirement_for_download(
            view.dataset.access.as_ref(),
            day.date,
            today,
            has_api_key,
            day.missing_keys.len(),
        )
    }

    fn open_api_key_prompt(&mut self, requirement: ApiKeyRequirement) {
        if self.api_key_prompt.is_none() {
            self.api_key_prompt = Some(ApiKeyPromptState {
                input: String::new(),
                error_message: None,
                access_message: requirement.message(),
            });
        }
    }

    fn close_api_key_prompt(&mut self) {
        self.api_key_prompt = None;
    }

    fn push_api_key_prompt_char(&mut self, c: char) {
        if let Some(prompt) = &mut self.api_key_prompt {
            prompt.input.push(c);
            prompt.error_message = None;
        }
    }

    fn pop_api_key_prompt_char(&mut self) {
        if let Some(prompt) = &mut self.api_key_prompt {
            prompt.input.pop();
            prompt.error_message = None;
        }
    }

    async fn submit_api_key_prompt(&mut self, client: &mut PolarisClient) -> Result<()> {
        let Some(prompt) = &mut self.api_key_prompt else {
            return Ok(());
        };

        let api_key = prompt.input.trim().to_string();
        if api_key.is_empty() {
            prompt.error_message = Some("API key cannot be empty".into());
            return Ok(());
        }

        let store = match KeychainCredentialStore::new() {
            Ok(store) => store,
            Err(err) => {
                prompt.error_message = Some(err.to_string());
                return Ok(());
            }
        };
        if let Err(err) = store.set_api_key(&api_key) {
            prompt.error_message = Some(err.to_string());
            return Ok(());
        }

        let config = match Config::from_env() {
            Ok(config) => config,
            Err(err) => {
                prompt.error_message = Some(err.to_string());
                return Ok(());
            }
        };
        *client = match PolarisClient::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.timeout,
        ) {
            Ok(client) => client,
            Err(err) => {
                prompt.error_message = Some(err.to_string());
                return Ok(());
            }
        };

        self.close_api_key_prompt();
        self.sync_selected_day(client).await
    }

    async fn pump_sync_updates(&mut self, client: &PolarisClient) -> Result<()> {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);

        let mut finished: Option<(String, NaiveDate, String)> = None;
        if let Some(sync) = &mut self.active_sync {
            let mut saw_progress_update = false;
            loop {
                let update = if let Some(update) = sync.deferred_update.take() {
                    Ok(update)
                } else {
                    sync.receiver.try_recv()
                };
                match update {
                    Ok(DaySyncUpdate::Started { total_bytes }) => {
                        sync.download_bytes = 0;
                        sync.download_total_bytes = total_bytes;
                        saw_progress_update = true;
                    }
                    Ok(DaySyncUpdate::Progress {
                        downloaded_bytes,
                        total_bytes,
                    }) => {
                        sync.download_bytes = downloaded_bytes;
                        if total_bytes.is_some() {
                            sync.download_total_bytes = total_bytes;
                        }
                        saw_progress_update = true;
                    }
                    Ok(DaySyncUpdate::Downloaded { total_bytes }) => {
                        sync.downloaded += 1;
                        sync.download_bytes = total_bytes;
                        sync.download_total_bytes = Some(total_bytes);
                        saw_progress_update = true;
                    }
                    Ok(DaySyncUpdate::Failed) => {
                        sync.failed += 1;
                        saw_progress_update = true;
                    }
                    Ok(DaySyncUpdate::Finished { downloaded, failed }) => {
                        if saw_progress_update {
                            sync.deferred_update =
                                Some(DaySyncUpdate::Finished { downloaded, failed });
                            break;
                        }
                        finished = Some((
                            sync.dataset.clone(),
                            sync.date,
                            format!("synced {} snapshot(s), failed {}", downloaded, failed),
                        ));
                        break;
                    }
                    Ok(DaySyncUpdate::Error(message)) => {
                        if saw_progress_update {
                            sync.deferred_update = Some(DaySyncUpdate::Error(message));
                            break;
                        }
                        finished = Some((
                            sync.dataset.clone(),
                            sync.date,
                            format!("error: {}", message),
                        ));
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some((
                            sync.dataset.clone(),
                            sync.date,
                            "error: sync task disconnected".into(),
                        ));
                        break;
                    }
                }
            }
        }

        if let Some((dataset_id, date, status)) = finished {
            self.active_sync = None;
            self.reload_local_state()?;
            if let Some(dataset) = self
                .datasets
                .iter()
                .find(|item| item.dataset == dataset_id)
                .cloned()
            {
                self.refresh_dataset_view(client, &dataset, date).await?;
            }
            self.status_message = Some(status);
        } else if let Some(sync) = &self.active_sync {
            let progress =
                format_byte_progress(sync.download_bytes, sync.download_total_bytes);
            self.status_message = Some(if progress.is_empty() {
                format!(
                    "syncing {} ({}/{})",
                    sync.date,
                    sync.local_present + sync.downloaded,
                    sync.remote_total
                )
            } else {
                format!(
                    "syncing {} ({}/{}, {})",
                    sync.date,
                    sync.local_present + sync.downloaded,
                    sync.remote_total,
                    progress
                )
            });
        }

        Ok(())
    }

    async fn refresh_dataset_view(
        &mut self,
        client: &PolarisClient,
        dataset: &RemoteDatasetEntry,
        selected_date: NaiveDate,
    ) -> Result<()> {
        let (remote_snapshots, _) = client
            .list_snapshots(
                &dataset.exchange,
                &dataset.asset,
                dataset.start,
                dataset.end,
            )
            .await?;
        let local_keys = self
            .local_keys
            .get(&dataset.dataset)
            .cloned()
            .unwrap_or_default();
        let days = build_day_coverages(
            remote_snapshots,
            &local_keys,
            dataset.start.date_naive(),
            dataset.end.date_naive(),
        );
        let selected_day = days
            .iter()
            .position(|day| day.date == selected_date)
            .unwrap_or_else(|| select_initial_day(&days));
        self.mode = ViewMode::Dataset(DatasetView {
            dataset: dataset.clone(),
            days,
            selected_day,
        });
        Ok(())
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
    fn selected_coverage(&self) -> &DayCoverage {
        &self.days[self.selected_day]
    }

    fn move_selection(&mut self, delta_days: i64) {
        if self.days.is_empty() {
            return;
        }
        let next = self.selected_day as i64 + delta_days;
        let max = (self.days.len() - 1) as i64;
        self.selected_day = next.clamp(0, max) as usize;
    }
}

impl DayCoverage {
    fn state(&self) -> DayState {
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

pub fn can_render_tui() -> bool {
    io::stdout().is_terminal() && io::stdin().is_terminal()
}

pub async fn run_remote_list_tui(
    client: PolarisClient,
    datasets: Vec<RemoteDatasetEntry>,
    local_snapshots: Vec<LocalSnapshotEntry>,
    root: PathBuf,
    concurrency: usize,
    seed: RemoteTuiSeed,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_event_loop(
        &mut terminal,
        client,
        RemoteListTui::new(
            datasets,
            local_snapshots,
            root,
            concurrency,
            seed,
        ),
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|err| TickError::Other(err.into()))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|err| TickError::Other(err.into()))?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(|err| TickError::Other(err.into()))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().map_err(|err| TickError::Other(err.into()))?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(|err| TickError::Other(err.into()))?;
    terminal
        .show_cursor()
        .map_err(|err| TickError::Other(err.into()))?;
    Ok(())
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: PolarisClient,
    mut app: RemoteListTui,
) -> Result<()> {
    let mut client = client;
    loop {
        app.pump_sync_updates(&client).await?;
        terminal
            .draw(|frame| render(frame, &app))
            .map_err(|err| TickError::Other(err.into()))?;

        if event::poll(Duration::from_millis(250)).map_err(|err| TickError::Other(err.into()))? {
            let event = event::read().map_err(|err| TickError::Other(err.into()))?;
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }
                if app.api_key_prompt.is_some() {
                    match key.code {
                        KeyCode::Esc => app.close_api_key_prompt(),
                        KeyCode::Enter => {
                            if let Err(err) = app.submit_api_key_prompt(&mut client).await {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        KeyCode::Backspace => app.pop_api_key_prompt_char(),
                        KeyCode::Char(c) if !c.is_control() => app.push_api_key_prompt_char(c),
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Esc => match app.mode {
                        ViewMode::Splash => {}
                        ViewMode::Browser => return Ok(()),
                        ViewMode::Dataset(_) => app.mode = ViewMode::Browser,
                    },
                    KeyCode::Char(' ') if matches!(app.mode, ViewMode::Splash) => {
                        app.mode = ViewMode::Browser;
                    }
                    KeyCode::Enter => {
                        if matches!(app.mode, ViewMode::Browser) {
                            if let Err(err) = app.open_selected_dataset(&client).await {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        } else if let Some(requirement) = app.api_key_requirement_for_selected_day(
                            Utc::now().date_naive(),
                            client.has_api_key(),
                        ) {
                            app.open_api_key_prompt(requirement);
                        } else if let Err(err) = app.sync_selected_day(&client).await {
                            app.status_message = Some(format!("error: {err}"));
                        }
                    }
                    KeyCode::Up => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(-7);
                        } else {
                            app.move_up();
                        }
                    }
                    KeyCode::Down => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(7);
                        } else {
                            app.move_down();
                        }
                    }
                    KeyCode::Left => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(-1);
                        } else if matches!(app.mode, ViewMode::Browser) {
                            app.cycle_category(-1);
                        }
                    }
                    KeyCode::Right => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(1);
                        } else if matches!(app.mode, ViewMode::Browser) {
                            app.cycle_category(1);
                        }
                    }
                    KeyCode::Tab => match app.mode {
                        ViewMode::Browser => {
                            if let Err(err) = app.toggle_current_bookmark() {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        ViewMode::Dataset(_) => {
                            if let Err(err) = app.reveal_selected_day_snapshot() {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        }
                        ViewMode::Splash => {}
                    },
                    KeyCode::Backspace => {
                        if matches!(app.mode, ViewMode::Browser) {
                            app.search.pop();
                            app.recompute_filter();
                        }
                    }
                    KeyCode::Char(c)
                        if matches!(app.mode, ViewMode::Browser) && is_search_input_key(&key) =>
                    {
                        if matches!(app.mode, ViewMode::Browser) {
                            app.search.push(c);
                            app.recompute_filter();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_splash(frame: &mut ratatui::Frame<'_>, spinner_tick: usize) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(3),
            Constraint::Length(6),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(area);

    let sky_area = centered_rect(92, sections[0].height, sections[0]);
    let copy_area = centered_rect(84, sections[1].height, sections[1]);
    let footer_area = centered_rect(64, sections[2].height, sections[2]);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(splash_sky_lines(sky_area, spinner_tick)).alignment(Alignment::Center),
        sky_area,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "Polaris",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "High-fidelity market data from Hyperliquid, Lighter, and more.",
                Style::default().fg(Color::White),
            )]),
            Line::from(vec![Span::styled(
                "Browse datasets. Track daily coverage. Pull the missing pieces.",
                Style::default().fg(Color::DarkGray),
            )]),
        ])
        .alignment(Alignment::Center),
        copy_area,
    );
    frame.render_widget(
        Paragraph::new(vec![Line::from(vec![Span::styled(
            " Space to open catalog ",
            Style::default().fg(Color::Black).bg(Color::White),
        )])])
        .alignment(Alignment::Center),
        footer_area,
    );
}

fn splash_sky_lines(area: Rect, spinner_tick: usize) -> Vec<Line<'static>> {
    let height = usize::from(area.height).max(8);
    let width = usize::from(area.width).max(24);
    let pole_x = (width as f32 * 0.58).min((width.saturating_sub(1)) as f32);
    let pole_y = (height as f32 * 0.30).max(1.0);
    let mut lines = Vec::with_capacity(height);

    for row in 0..height {
        lines.push(splash_sky_line(
            width,
            height,
            row,
            pole_x,
            pole_y,
            spinner_tick,
        ));
    }

    lines
}

fn splash_sky_line(
    width: usize,
    height: usize,
    row: usize,
    pole_x: f32,
    pole_y: f32,
    spinner_tick: usize,
) -> Line<'static> {
    if row + 1 >= height {
        return Line::from(" ".repeat(width));
    }

    let mut spans = Vec::with_capacity(width);

    for col in 0..width {
        spans.push(splash_sky_cell(
            width,
            height,
            col,
            row,
            pole_x,
            pole_y,
            spinner_tick,
        ));
    }

    Line::from(spans)
}

fn splash_sky_cell(
    width: usize,
    height: usize,
    col: usize,
    row: usize,
    pole_x: f32,
    pole_y: f32,
    spinner_tick: usize,
) -> Span<'static> {
    let x = col as f32;
    let y = row as f32;
    let dx = (x - pole_x) * 0.48;
    let dy = y - pole_y;
    let radius = (dx * dx + dy * dy).sqrt();
    let ring_phase = radius * 1.33;
    let ring_error = (ring_phase - ring_phase.round()).abs();
    let angle = dy.atan2(dx);
    let sweep = angle * 4.0 + spinner_tick as f32 * 0.18 + radius * 0.09;
    let sparkle = sweep.sin().abs();
    let horizon_fade = 1.0 - (row as f32 / height.max(1) as f32) * 0.35;

    if radius < 1.4 {
        return Span::styled(
            "*",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    }

    if ring_error > 0.12 * horizon_fade {
        return Span::raw(" ");
    }

    let color = if sparkle > 0.96 {
        Color::White
    } else if sparkle > 0.86 {
        Color::Cyan
    } else if radius < width as f32 * 0.10 {
        Color::Gray
    } else if radius < width as f32 * 0.22 {
        Color::DarkGray
    } else {
        Color::Gray
    };
    let symbol = if sparkle > 0.97 {
        "*"
    } else if sparkle > 0.90 {
        ":"
    } else if ((col + row + spinner_tick / 2) % 11) == 0 {
        "'"
    } else {
        "."
    };

    Span::styled(symbol, Style::default().fg(color))
}
fn render(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    match app.dataset_view() {
        Some(view) => render_dataset_view(
            frame,
            view,
            app.is_bookmarked(view.dataset.dataset.as_str()),
            app.status_message.as_deref(),
            app.active_sync.as_ref(),
            app.spinner_tick,
        ),
        None => match app.mode {
            ViewMode::Splash => render_splash(frame, app.spinner_tick),
            ViewMode::Browser => render_browser(frame, app),
            _ => unreachable!(),
        },
    }

    if let Some(prompt) = &app.api_key_prompt {
        render_api_key_prompt(frame, prompt);
    }
}

fn render_browser(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let version_title = Title {
        content: Line::from("Polaris v0.1.0").alignment(Alignment::Right),
        alignment: Some(Alignment::Right),
        position: None,
    };
    let search = Paragraph::new(app.search.clone()).block(
        Block::default()
            .title("Search dataset or access")
            .title(version_title)
            .borders(Borders::ALL),
    );
    frame.render_widget(search, areas[0]);

    let items = if app.filtered_indices.is_empty() {
        vec![ListItem::new("No datasets match the current search")]
    } else {
        app.filtered_indices
            .iter()
            .map(|index| {
                let dataset = &app.datasets[*index];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<12} ", dataset.access_badge()),
                        Style::default().fg(access_color(dataset.access.as_ref())),
                    ),
                    Span::styled(
                        if app.is_bookmarked(dataset.dataset.as_str()) {
                            "* "
                        } else {
                            "  "
                        },
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        dataset.dataset.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect()
    };

    let mut state = ListState::default()
        .with_selected((!app.filtered_indices.is_empty()).then_some(app.selected));
    let list = List::new(items)
        .block(
            Block::default()
                .title("Datasets")
                .title(category_carousel_title(app))
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, areas[1], &mut state);

    render_footer(
        frame,
        areas[2],
        " Type to search  │  ←/→ category  │  ↑/↓ navigate  │  Tab bookmark  │  Enter inspect dataset  │  Ctrl+C quit ",
    );
}

fn category_carousel_title(app: &RemoteListTui) -> Title<'static> {
    let labels = app.category_display_labels();
    let mut spans = Vec::new();

    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        }

        let is_active = index == 0;
        let text = if is_active {
            format!("*{label}*")
        } else {
            label.clone()
        };
        let style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(text, style));
    }

    Title {
        content: Line::from(spans).alignment(Alignment::Center),
        alignment: Some(Alignment::Center),
        position: None,
    }
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, text: &str) {
    let footer = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, area);
}

fn render_dataset_view(
    frame: &mut ratatui::Frame<'_>,
    view: &DatasetView,
    is_bookmarked: bool,
    status_message: Option<&str>,
    active_sync: Option<&ActiveDaySync>,
    spinner_tick: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(frame.area());

    frame.render_widget(
        render_day_grid(view, is_bookmarked, active_sync, spinner_tick),
        areas[0],
    );
    frame.render_widget(
        render_selected_day_summary(view, active_sync, status_message),
        areas[1],
    );

    render_footer(
        frame,
        areas[2],
        " Enter sync day  │  Tab Show in Finder  │  ←/→ move day  │  ↑/↓ move week  │  Esc back  │  Ctrl+C quit ",
    );
}

fn render_day_grid(
    view: &DatasetView,
    is_bookmarked: bool,
    active_sync: Option<&ActiveDaySync>,
    spinner_tick: usize,
) -> Paragraph<'static> {
    let selected_date = view.selected_coverage().date;
    let mut lines = Vec::new();
    let mut month_start = 0usize;

    while month_start < view.days.len() {
        let month = view.days[month_start].date.month();
        let year = view.days[month_start].date.year();
        let mut month_end = month_start;
        while month_end < view.days.len()
            && view.days[month_end].date.month() == month
            && view.days[month_end].date.year() == year
        {
            month_end += 1;
        }

        if !lines.is_empty() {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(vec![Span::styled(
            view.days[month_start].date.format("%B %Y").to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        lines.push(weekday_header_line());

        let month_days = &view.days[month_start..month_end];
        let month_first = month_days
            .first()
            .map(|day| day.date)
            .expect("month section requires at least one day");
        let month_last = month_days
            .last()
            .map(|day| day.date)
            .expect("month section requires at least one day");
        let grid_start =
            month_first - ChronoDuration::days(month_first.weekday().num_days_from_monday() as i64);
        let grid_end = month_last
            + ChronoDuration::days((6 - month_last.weekday().num_days_from_monday()) as i64);

        let mut cursor = grid_start;
        while cursor <= grid_end {
            let mut spans = Vec::with_capacity(7);
            for _ in 0..7 {
                if let Some(day) = month_days.iter().find(|day| day.date == cursor) {
                    spans.push(render_day_cell(
                        day,
                        cursor == selected_date,
                        active_sync,
                        &view.dataset.dataset,
                        spinner_tick,
                    ));
                } else {
                    spans.push(Span::styled(
                        "        ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                cursor += ChronoDuration::days(1);
            }
            lines.push(Line::from(spans));
        }

        month_start = month_end;
    }

    let mut dataset_title_spans = vec![Span::styled(
        view.dataset.access_badge(),
        Style::default().fg(access_color(view.dataset.access.as_ref())),
    )];
    if is_bookmarked {
        dataset_title_spans.push(Span::styled(" *", Style::default().fg(Color::Yellow)));
    }
    dataset_title_spans.push(Span::raw(" "));
    dataset_title_spans.push(Span::styled(
        view.dataset.dataset.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));

    Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .title("Daily Coverage")
            .title(Title {
                content: Line::from(dataset_title_spans).alignment(Alignment::Right),
                alignment: Some(Alignment::Right),
                position: None,
            })
            .borders(Borders::ALL),
    )
}

fn weekday_header_line() -> Line<'static> {
    Line::from(vec![
        Span::raw(" Mon    "),
        Span::raw(" Tue    "),
        Span::raw(" Wed    "),
        Span::raw(" Thu    "),
        Span::raw(" Fri    "),
        Span::raw(" Sat    "),
        Span::raw(" Sun"),
    ])
}

fn render_day_cell(
    day: &DayCoverage,
    selected: bool,
    active_sync: Option<&ActiveDaySync>,
    dataset: &str,
    spinner_tick: usize,
) -> Span<'static> {
    let status = if active_sync
        .map(|sync| sync.dataset == dataset && sync.date == day.date)
        .unwrap_or(false)
    {
        spinner_frame(spinner_tick).to_string()
    } else {
        match day.state() {
            DayState::Full => "OK".to_string(),
            DayState::Partial => format!("~{}", compact_count(day.missing_keys.len())),
            DayState::Empty => "--".to_string(),
            DayState::NoRemote => "..".to_string(),
        }
    };
    let style = match day.state() {
        DayState::Full => Style::default().fg(Color::Green),
        DayState::Partial => Style::default().fg(Color::Yellow),
        DayState::Empty => Style::default().fg(Color::Red),
        DayState::NoRemote => Style::default().fg(Color::DarkGray),
    };
    let style = if selected {
        style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        style
    };
    Span::styled(format!("{:>2} {:<4} ", day.date.day(), status), style)
}

fn render_selected_day_summary(
    view: &DatasetView,
    active_sync: Option<&ActiveDaySync>,
    status_message: Option<&str>,
) -> Paragraph<'static> {
    let day = view.selected_coverage();
    let (remote_total, local_total, missing_total, state) =
        sync_adjusted_day_totals(view, day, active_sync);
    let completion_bar = render_completion_bar(local_total, remote_total, 18);
    let state_style = match state {
        "full" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        "partial" => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        "none local" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "no remote data" => Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        "syncing" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::BOLD),
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                day.date.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(state.to_ascii_uppercase(), state_style),
        ]),
        Line::from(format!(
            "coverage: {}   missing: {}",
            completion_bar, missing_total
        )),
        Line::from(format_snapshot_location(view, day)),
    ];
    if let Some(status) = status_message {
        lines.push(Line::from(vec![
            Span::styled("status: ", Style::default().fg(Color::DarkGray)),
            Span::raw(status.to_string()),
        ]));
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().title("Selected Day").borders(Borders::ALL))
}

fn format_snapshot_location(view: &DatasetView, day: &DayCoverage) -> String {
    let key = day.local_keys.first().or_else(|| day.remote_keys.first());
    let path = key
        .and_then(|key| AppLayout::new(PathBuf::new()).data_path_for_key(key).ok())
        .map(|path| {
            path.parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| "data".into())
        })
        .unwrap_or_else(|| {
            format!(
                "data/<source>/{}/{}/{}",
                view.dataset.exchange, view.dataset.asset, day.date
            )
        });
    if day.local_keys.is_empty() {
        format!("will store under: {path}")
    } else {
        format!("stored under: {path}")
    }
}

fn snapshot_reveal_target(data_root: &Path, snapshot_paths: &[PathBuf]) -> Option<FileManagerTarget> {
    for path in snapshot_paths {
        if path.is_file() {
            return Some(FileManagerTarget::File(path.clone()));
        }
        if path.is_dir() {
            return Some(FileManagerTarget::Directory(path.clone()));
        }

        let mut parent = path.parent();
        while let Some(dir) = parent {
            if dir.is_dir() {
                return Some(FileManagerTarget::Directory(dir.to_path_buf()));
            }
            if dir == data_root {
                break;
            }
            parent = dir.parent();
        }
    }

    if data_root.is_dir() {
        return Some(FileManagerTarget::Directory(data_root.to_path_buf()));
    }

    None
}
fn open_in_file_manager(target: &FileManagerTarget) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        match target {
            FileManagerTarget::File(path) => {
                command.arg("-R").arg(path);
            }
            FileManagerTarget::Directory(path) => {
                command.arg(path);
            }
        }
        command
            .spawn()
            .with_context(|| "failed to launch Finder".to_string())
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        match target {
            FileManagerTarget::File(path) => {
                command.arg(format!("/select,{}", path.display()));
            }
            FileManagerTarget::Directory(path) => {
                command.arg(path);
            }
        }
        command
            .spawn()
            .with_context(|| "failed to launch Explorer".to_string())
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let path = match target {
            FileManagerTarget::File(path) => path.parent().unwrap_or(path),
            FileManagerTarget::Directory(path) => path.as_path(),
        };
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .with_context(|| "failed to launch file manager".to_string())
            .map_err(TickError::Other)?;
        Ok(())
    }
}

fn render_api_key_prompt(frame: &mut ratatui::Frame<'_>, prompt: &ApiKeyPromptState) {
    let area = centered_rect(72, 10, frame.area());
    let masked_input = if prompt.input.is_empty() {
        "<empty>".to_string()
    } else {
        "*".repeat(prompt.input.chars().count())
    };

    let mut lines = vec![
        Line::from(vec![Span::styled(
            prompt.access_message.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("Go to polaris.supply to grab your API key."),
        Line::from(""),
        Line::from(format!("API key: {masked_input}")),
        Line::from("Enter saves the key and continues syncing. Esc cancels."),
    ];
    if let Some(error) = &prompt.error_message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title("Polaris API Key")
                    .borders(Borders::ALL),
            ),
        area,
    );
}

fn summarize_local_snapshots(
    snapshots: &[LocalSnapshotEntry],
) -> BTreeMap<String, LocalDatasetSummary> {
    let mut summaries = BTreeMap::new();
    for snapshot in snapshots {
        let (Some(exchange), Some(asset)) =
            (snapshot.exchange.as_deref(), snapshot.asset.as_deref())
        else {
            continue;
        };
        let key = format!("{exchange}:{asset}");
        let summary = summaries
            .entry(key)
            .or_insert_with(LocalDatasetSummary::default);
        summary.snapshot_count += 1;
        if let Some(start) = snapshot.start {
            summary.first_start = match summary.first_start {
                Some(current) => Some(current.min(start)),
                None => Some(start),
            };
        }
        if let Some(end) = snapshot.end {
            summary.last_end = match summary.last_end {
                Some(current) => Some(current.max(end)),
                None => Some(end),
            };
        }
    }
    summaries
}

fn group_local_snapshot_keys(snapshots: Vec<LocalSnapshotEntry>) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::new();
    for snapshot in snapshots {
        let (Some(exchange), Some(asset)) = (snapshot.exchange.clone(), snapshot.asset.clone())
        else {
            continue;
        };
        grouped
            .entry(format!("{exchange}:{asset}"))
            .or_insert_with(Vec::new)
            .push(snapshot.key);
    }
    for keys in grouped.values_mut() {
        keys.sort();
    }
    grouped
}

fn diff_missing_snapshot_keys(remote_keys: Vec<String>, local_keys: &[String]) -> Vec<String> {
    let local_set = local_keys.iter().collect::<BTreeSet<_>>();
    remote_keys
        .into_iter()
        .filter(|key| !local_set.contains(key))
        .collect()
}

fn build_day_coverages(
    remote_snapshots: Vec<SnapshotEntry>,
    local_keys: &[String],
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Vec<DayCoverage> {
    let mut days = Vec::new();
    let mut current = start_date;
    while current <= end_date {
        days.push(DayCoverage {
            date: current,
            remote_keys: Vec::new(),
            local_keys: Vec::new(),
            missing_keys: Vec::new(),
        });
        current += ChronoDuration::days(1);
    }

    for snapshot in remote_snapshots {
        if let Some(date) = snapshot_date_from_key(&snapshot.key) {
            if date >= start_date && date <= end_date {
                let offset = (date - start_date).num_days() as usize;
                days[offset].remote_keys.push(snapshot.key);
            }
        }
    }

    for key in local_keys {
        if let Some(date) = snapshot_date_from_key(key) {
            if date >= start_date && date <= end_date {
                let offset = (date - start_date).num_days() as usize;
                days[offset].local_keys.push(key.clone());
            }
        }
    }

    for day in &mut days {
        day.remote_keys.sort();
        day.local_keys.sort();
        day.missing_keys = diff_missing_snapshot_keys(day.remote_keys.clone(), &day.local_keys);
    }

    days
}

fn select_initial_day(days: &[DayCoverage]) -> usize {
    days.iter()
        .position(|day| !day.missing_keys.is_empty())
        .unwrap_or(0)
}

fn snapshot_date_from_key(key: &str) -> Option<NaiveDate> {
    infer_snapshot_date_from_key(key)
}

fn compact_count(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

fn format_byte_progress(downloaded_bytes: u64, total_bytes: Option<u64>) -> String {
    match total_bytes {
        Some(total_bytes) if total_bytes > 0 => {
            format!(
                "{} / {}",
                format_bytes(downloaded_bytes.min(total_bytes)),
                format_bytes(total_bytes)
            )
        }
        Some(_) | None if downloaded_bytes > 0 => format_bytes(downloaded_bytes),
        _ => String::new(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for candidate in UNITS {
        unit = candidate;
        if value < 1024.0 || candidate == UNITS[UNITS.len() - 1] {
            break;
        }
        value /= 1024.0;
    }

    if unit == "B" {
        format!("{bytes} {unit}")
    } else if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

fn spinner_frame(tick: usize) -> &'static str {
    match tick % 4 {
        0 => "|",
        1 => "/",
        2 => "-",
        _ => "\\",
    }
}

fn sync_adjusted_day_totals<'a>(
    view: &DatasetView,
    day: &'a DayCoverage,
    active_sync: Option<&ActiveDaySync>,
) -> (usize, usize, usize, &'a str) {
    if let Some(sync) = active_sync {
        if sync.dataset == view.dataset.dataset && sync.date == day.date {
            let local_total = (sync.local_present + sync.downloaded).min(sync.remote_total);
            let missing_total = sync.remote_total.saturating_sub(local_total);
            let state = "syncing";
            return (sync.remote_total, local_total, missing_total, state);
        }
    }

    let state = match day.state() {
        DayState::Full => "full",
        DayState::Partial => "partial",
        DayState::Empty => "none local",
        DayState::NoRemote => "no remote data",
    };
    (
        day.remote_keys.len(),
        day.local_keys.len(),
        day.missing_keys.len(),
        state,
    )
}

fn render_completion_bar(local_total: usize, remote_total: usize, width: usize) -> String {
    if remote_total == 0 {
        return "[........................] no remote data".to_string();
    }

    let filled = ((local_total.min(remote_total) * width) + (remote_total / 2)) / remote_total;
    let mut bar = String::with_capacity(width + 16);
    bar.push('[');
    for idx in 0..width {
        if idx < filled {
            bar.push('#');
        } else {
            bar.push('.');
        }
    }
    bar.push(']');
    bar.push(' ');
    bar.push_str(&format!(
        "{}/{}",
        local_total.min(remote_total),
        remote_total
    ));
    bar
}

fn centered_rect(width_percentage: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Percentage(width_percentage.min(100)),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn is_search_input_key(key: &crossterm::event::KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };

    let allowed_modifiers = KeyModifiers::NONE | KeyModifiers::SHIFT;
    if !(key.modifiers - allowed_modifiers).is_empty() {
        return false;
    }

    c == ' ' || c.is_ascii_graphic()
}

fn bookmarks_path(root: &Path) -> PathBuf {
    root.join("bookmarks.json")
}

fn load_bookmarks(root: &Path) -> Result<BTreeSet<String>> {
    let path = bookmarks_path(root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(err) => return Err(TickError::Other(err.into())),
    };

    let store: BookmarkStore = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map_err(TickError::Other)?;
    Ok(store.bookmarks)
}

fn save_bookmarks(root: &Path, bookmarks: &BTreeSet<String>) -> Result<()> {
    fs::create_dir_all(root).map_err(|err| TickError::Other(err.into()))?;
    let path = bookmarks_path(root);
    let contents = serde_json::to_string_pretty(&BookmarkStore {
        bookmarks: bookmarks.clone(),
    })
    .with_context(|| format!("failed to serialize {}", path.display()))
    .map_err(TickError::Other)?;
    fs::write(&path, contents).map_err(|err| TickError::Other(err.into()))
}

fn access_color(access: Option<&DatasetAccess>) -> Color {
    match access.map(|item| &item.status) {
        Some(DatasetAccessStatus::Open) => Color::Green,
        Some(DatasetAccessStatus::Preview) => Color::Yellow,
        Some(DatasetAccessStatus::Restricted) => Color::Red,
        None => Color::DarkGray,
    }
}

fn browser_categories(datasets: &[RemoteDatasetEntry]) -> Vec<BrowserCategory> {
    let mut categories = vec![BrowserCategory::AllDatasets, BrowserCategory::Bookmarks];
    let catalog_categories = datasets
        .iter()
        .flat_map(|dataset| dataset.categories.iter().cloned())
        .collect::<BTreeSet<_>>();
    categories.extend(catalog_categories.into_iter().map(BrowserCategory::Catalog));
    categories
}

fn api_key_requirement_for_download(
    access: Option<&DatasetAccess>,
    selected_date: NaiveDate,
    today: NaiveDate,
    has_api_key: bool,
    missing_snapshot_count: usize,
) -> Option<ApiKeyRequirement> {
    if has_api_key || missing_snapshot_count == 0 {
        return None;
    }

    match access {
        Some(access) if !access.requires_api_key_for_date(selected_date) => None,
        Some(DatasetAccess {
            status: DatasetAccessStatus::Restricted,
            ..
        }) => Some(ApiKeyRequirement::Restricted),
        Some(DatasetAccess {
            status: DatasetAccessStatus::Preview,
            public_cutoff_date,
        }) => Some(ApiKeyRequirement::Preview {
            public_cutoff_date: *public_cutoff_date,
        }),
        Some(DatasetAccess {
            status: DatasetAccessStatus::Open,
            ..
        }) => None,
        None if selected_date < today - ChronoDuration::days(7) => {
            Some(ApiKeyRequirement::LegacyPreviewWindow)
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::NaiveDate;
    use chrono::{TimeZone, Utc};
    use tokio::sync::mpsc::unbounded_channel;

    use super::{
        ActiveDaySync, ApiKeyRequirement, BrowserCategory, DatasetView, DaySyncUpdate,
        FileManagerTarget, RemoteDatasetEntry, RemoteListTui, RemoteTuiSeed,
        api_key_requirement_for_download, build_day_coverages, diff_missing_snapshot_keys,
        format_snapshot_location, load_bookmarks, save_bookmarks, snapshot_reveal_target,
    };
    use crate::api::{DatasetAccess, DatasetAccessStatus, PolarisClient, SnapshotEntry};
    use crate::layout::LocalSnapshotEntry;

    #[test]
    fn search_filters_remote_datasets() {
        let datasets = vec![
            RemoteDatasetEntry {
                exchange: "aster".into(),
                asset: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Open,
                    public_cutoff_date: None,
                }),
                categories: Vec::new(),
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                exchange: "binance".into(),
                asset: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Restricted,
                    public_cutoff_date: None,
                }),
                categories: Vec::new(),
                dataset: "binance:ETHUSDT".into(),
            },
        ];

        let app = RemoteListTui::new(
            datasets,
            Vec::<LocalSnapshotEntry>::new(),
            PathBuf::from("/tmp/tick"),
            4,
            RemoteTuiSeed {
                search: Some("btc".into()),
                ..RemoteTuiSeed::default()
            },
        );

        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some("aster:BTCUSDT")
        );
    }

    #[test]
    fn access_search_filters_remote_datasets() {
        let datasets = vec![
            RemoteDatasetEntry {
                exchange: "aster".into(),
                asset: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Preview,
                    public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
                }),
                categories: Vec::new(),
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                exchange: "binance".into(),
                asset: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Restricted,
                    public_cutoff_date: None,
                }),
                categories: Vec::new(),
                dataset: "binance:ETHUSDT".into(),
            },
        ];

        let app = RemoteListTui::new(
            datasets,
            Vec::<LocalSnapshotEntry>::new(),
            PathBuf::from("/tmp/tick"),
            4,
            RemoteTuiSeed {
                search: Some("access:restricted".into()),
                ..RemoteTuiSeed::default()
            },
        );

        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some("binance:ETHUSDT")
        );
    }

    #[test]
    fn bookmarked_datasets_sort_to_top() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let bookmarked = "binance:ETHUSDT".to_string();
        save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()]))
            .expect("save bookmarks");

        let app = RemoteListTui::new(
            vec![
                RemoteDatasetEntry {
                    exchange: "aster".into(),
                    asset: "BTCUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: Vec::new(),
                    dataset: "aster:BTCUSDT".into(),
                },
                RemoteDatasetEntry {
                    exchange: "binance".into(),
                    asset: "ETHUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: Vec::new(),
                    dataset: bookmarked.clone(),
                },
            ],
            Vec::<LocalSnapshotEntry>::new(),
            tempdir.path().to_path_buf(),
            4,
            RemoteTuiSeed::default(),
        );

        assert_eq!(app.filtered_indices, vec![1, 0]);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some(bookmarked.as_str())
        );
    }

    #[test]
    fn toggling_bookmark_persists_without_reordering_current_session() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let bookmarked = "binance:ETHUSDT".to_string();
        let mut app = RemoteListTui::new(
            vec![
                RemoteDatasetEntry {
                    exchange: "aster".into(),
                    asset: "BTCUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: Vec::new(),
                    dataset: "aster:BTCUSDT".into(),
                },
                RemoteDatasetEntry {
                    exchange: "binance".into(),
                    asset: "ETHUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: Vec::new(),
                    dataset: bookmarked.clone(),
                },
            ],
            Vec::<LocalSnapshotEntry>::new(),
            tempdir.path().to_path_buf(),
            4,
            RemoteTuiSeed::default(),
        );

        app.selected = 1;
        app.toggle_current_bookmark().expect("toggle bookmark");

        assert_eq!(app.filtered_indices, vec![0, 1]);
        assert_eq!(app.selected, 1);
        assert!(app.is_bookmarked(bookmarked.as_str()));
        assert_eq!(
            load_bookmarks(tempdir.path()).expect("load bookmarks"),
            BTreeSet::from([bookmarked])
        );
    }

    #[test]
    fn category_carousel_cycles_through_bookmarks_and_catalog_categories() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let bookmarked = "binance:ETHUSDT".to_string();
        save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()]))
            .expect("save bookmarks");

        let mut app = RemoteListTui::new(
            vec![
                RemoteDatasetEntry {
                    exchange: "aster".into(),
                    asset: "BTCUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: vec!["Spot".into()],
                    dataset: "aster:BTCUSDT".into(),
                },
                RemoteDatasetEntry {
                    exchange: "binance".into(),
                    asset: "ETHUSDT".into(),
                    start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                    source: Some("manifest".into()),
                    access: Some(DatasetAccess {
                        status: DatasetAccessStatus::Open,
                        public_cutoff_date: None,
                    }),
                    categories: vec!["Futures".into()],
                    dataset: bookmarked.clone(),
                },
            ],
            Vec::<LocalSnapshotEntry>::new(),
            tempdir.path().to_path_buf(),
            4,
            RemoteTuiSeed::default(),
        );

        assert_eq!(
            app.categories
                .iter()
                .map(BrowserCategory::label)
                .collect::<Vec<_>>(),
            vec!["All", "Bookmarks", "Futures", "Spot"]
        );
        assert_eq!(app.selected_category().label(), "All");
        assert_eq!(
            app.category_display_labels(),
            vec!["all", "bookmarks", "futures", "spot"]
        );

        app.cycle_category(-1);
        assert_eq!(app.selected_category().label(), "Spot");
        assert_eq!(
            app.category_display_labels(),
            vec!["spot", "all", "bookmarks", "futures"]
        );
        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some("aster:BTCUSDT")
        );

        app.cycle_category(1);
        assert_eq!(app.selected_category().label(), "All");
        assert_eq!(
            app.category_display_labels(),
            vec!["all", "bookmarks", "futures", "spot"]
        );

        app.cycle_category(1);
        assert_eq!(app.selected_category().label(), "Bookmarks");
        assert_eq!(
            app.category_display_labels(),
            vec!["bookmarks", "futures", "spot", "all"]
        );
        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some(bookmarked.as_str())
        );

        app.cycle_category(1);
        assert_eq!(app.selected_category().label(), "Futures");
        assert_eq!(
            app.category_display_labels(),
            vec!["futures", "spot", "all", "bookmarks"]
        );
        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some(bookmarked.as_str())
        );

        app.cycle_category(1);
        assert_eq!(app.selected_category().label(), "Spot");
        assert_eq!(
            app.category_display_labels(),
            vec!["spot", "all", "bookmarks", "futures"]
        );
        assert_eq!(app.filtered_indices.len(), 1);
        assert_eq!(
            app.selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
            Some("aster:BTCUSDT")
        );
    }

    #[test]
    fn removing_bookmark_refreshes_bookmarks_category() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let bookmarked = "binance:ETHUSDT".to_string();
        save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()]))
            .expect("save bookmarks");

        let mut app = RemoteListTui::new(
            vec![RemoteDatasetEntry {
                exchange: "binance".into(),
                asset: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Open,
                    public_cutoff_date: None,
                }),
                categories: vec!["Futures".into()],
                dataset: bookmarked.clone(),
            }],
            Vec::<LocalSnapshotEntry>::new(),
            tempdir.path().to_path_buf(),
            4,
            RemoteTuiSeed::default(),
        );

        app.cycle_category(1);
        assert_eq!(app.selected_category().label(), "Bookmarks");
        assert_eq!(app.filtered_indices.len(), 1);

        app.toggle_current_bookmark().expect("toggle bookmark");

        assert!(app.filtered_indices.is_empty());
        assert!(app.selected_dataset().is_none());
        assert!(!app.is_bookmarked(bookmarked.as_str()));
    }

    #[test]
    fn diff_marks_only_missing_remote_keys() {
        let missing = diff_missing_snapshot_keys(
            vec!["a".into(), "b".into(), "c".into()],
            &["a".into(), "c".into()],
        );
        assert_eq!(missing, vec!["b"]);
    }

    #[test]
    fn day_coverages_classify_full_partial_and_empty_days() {
        let remote = vec![
            SnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-01/a.jsonl.zst".into(),
                filename: "a.jsonl.zst".into(),
            },
            SnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-01/b.jsonl.zst".into(),
                filename: "b.jsonl.zst".into(),
            },
            SnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-02/c.jsonl.zst".into(),
                filename: "c.jsonl.zst".into(),
            },
            SnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-03/d.jsonl.zst".into(),
                filename: "d.jsonl.zst".into(),
            },
        ];
        let local = vec![
            "bronze/aster/BTCUSDT/2026-06-01/a.jsonl.zst".into(),
            "bronze/aster/BTCUSDT/2026-06-01/b.jsonl.zst".into(),
            "bronze/aster/BTCUSDT/2026-06-02/c.jsonl.zst".into(),
        ];

        let days = build_day_coverages(
            remote,
            &local,
            Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
                .unwrap()
                .date_naive(),
            Utc.with_ymd_and_hms(2026, 6, 4, 0, 0, 0)
                .unwrap()
                .date_naive(),
        );

        assert_eq!(days[0].missing_keys.len(), 0);
        assert_eq!(days[1].missing_keys.len(), 0);
        assert_eq!(days[2].missing_keys.len(), 1);
        assert_eq!(days[3].remote_keys.len(), 0);
    }

    #[test]
    fn selected_day_summary_reports_snapshot_location() {
        let dataset = RemoteDatasetEntry {
            exchange: "aster".into(),
            asset: "BTCUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            source: Some("manifest".into()),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Open,
                public_cutoff_date: None,
            }),
            categories: Vec::new(),
            dataset: "aster:BTCUSDT".into(),
        };
        let date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let mut days = build_day_coverages(
            Vec::new(),
            &[],
            date,
            date,
        );
        let view = DatasetView {
            dataset: dataset.clone(),
            days: days.clone(),
            selected_day: 0,
        };
        assert_eq!(
            format_snapshot_location(&view, &view.days[0]),
            "will store under: data/<source>/aster/BTCUSDT/2026-06-01"
        );

        days[0].local_keys = vec!["bronze/aster/BTCUSDT/2026-06-01/a.jsonl.zst".into()];
        let view = DatasetView {
            dataset,
            days,
            selected_day: 0,
        };
        assert_eq!(
            format_snapshot_location(&view, &view.days[0]),
            "stored under: data/bronze/aster/BTCUSDT/2026-06-01"
        );
    }

    #[test]
    fn reveal_target_prefers_exact_snapshot_file() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let data_root = tempdir.path().join("data");
        let snapshot_path = data_root.join("bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst");
        std::fs::create_dir_all(snapshot_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&snapshot_path, b"snapshot").expect("write");

        assert_eq!(
            snapshot_reveal_target(&data_root, std::slice::from_ref(&snapshot_path)),
            Some(FileManagerTarget::File(snapshot_path))
        );
    }

    #[test]
    fn reveal_target_falls_back_to_snapshot_directory_when_file_is_missing() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let data_root = tempdir.path().join("data");
        let snapshot_path = data_root.join("bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst");
        let day_dir = data_root.join("bronze/aster/BTCUSDT/2026-06-01");
        std::fs::create_dir_all(&day_dir).expect("mkdir");

        assert_eq!(
            snapshot_reveal_target(&data_root, std::slice::from_ref(&snapshot_path)),
            Some(FileManagerTarget::Directory(day_dir))
        );
    }

    #[test]
    fn reveal_target_falls_back_to_data_root_when_no_snapshot_parents_exist() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let data_root = tempdir.path().join("data");
        std::fs::create_dir_all(&data_root).expect("mkdir");
        let snapshot_path = data_root.join("bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst");

        assert_eq!(
            snapshot_reveal_target(&data_root, &[snapshot_path]),
            Some(FileManagerTarget::Directory(data_root))
        );
    }

    #[tokio::test]
    async fn sync_updates_do_not_skip_progress_frames() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let dataset = RemoteDatasetEntry {
            exchange: "aster".into(),
            asset: "ASTERUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 1, 23, 59, 59).unwrap(),
            source: Some("manifest".into()),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
            }),
            categories: Vec::new(),
            dataset: "aster:ASTERUSDT".into(),
        };
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let mut app = RemoteListTui::new(
            vec![dataset.clone()],
            Vec::<LocalSnapshotEntry>::new(),
            tempdir.path().to_path_buf(),
            4,
            RemoteTuiSeed::default(),
        );
        app.mode = super::ViewMode::Dataset(DatasetView {
            dataset: dataset.clone(),
            days: build_day_coverages(
                vec![SnapshotEntry {
                    key: "snapshots/standard/aster/ASTERUSDT/2026-06-01.jsonl.zst".into(),
                    filename: "aster_ASTERUSDT_2026-06-01_standard.jsonl.zst".into(),
                }],
                &[],
                date,
                date,
            ),
            selected_day: 0,
        });
        let (tx, rx) = unbounded_channel();
        tx.send(DaySyncUpdate::Started {
            total_bytes: Some(2048),
        })
        .expect("started");
        tx.send(DaySyncUpdate::Progress {
            downloaded_bytes: 1024,
            total_bytes: Some(2048),
        })
        .expect("progress");
        tx.send(DaySyncUpdate::Downloaded { total_bytes: 2048 })
            .expect("downloaded");
        tx.send(DaySyncUpdate::Finished {
            downloaded: 1,
            failed: 0,
        })
        .expect("finished");
        drop(tx);
        app.active_sync = Some(ActiveDaySync {
            dataset: dataset.dataset.clone(),
            date,
            remote_total: 1,
            local_present: 0,
            downloaded: 0,
            failed: 0,
            download_bytes: 0,
            download_total_bytes: None,
            deferred_update: None,
            receiver: rx,
        });
        let client = PolarisClient::new(
            "https://api.polaris.supply".into(),
            None,
            Duration::from_secs(1),
        )
        .expect("client");

        app.pump_sync_updates(&client).await.expect("first pump");
        let sync = app.active_sync.as_ref().expect("sync still active");
        assert_eq!(sync.downloaded, 1);
        assert_eq!(sync.download_bytes, 2048);
        assert_eq!(sync.download_total_bytes, Some(2048));
        assert!(matches!(sync.deferred_update, Some(DaySyncUpdate::Finished { .. })));
        assert_eq!(
            app.status_message.as_deref(),
            Some("syncing 2026-06-01 (1/1, 2.00 KiB / 2.00 KiB)")
        );

        app.pump_sync_updates(&client).await.expect("second pump");
        assert_eq!(
            app.status_message.as_deref(),
            Some("synced 1 snapshot(s), failed 0")
        );
        assert!(app.active_sync.is_none());
    }

    #[test]
    fn restricted_datasets_without_api_key_require_prompt() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 2).unwrap();
        assert_eq!(
            api_key_requirement_for_download(
                Some(&DatasetAccess {
                    status: DatasetAccessStatus::Restricted,
                    public_cutoff_date: None,
                }),
                selected_date,
                today,
                false,
                1
            ),
            Some(ApiKeyRequirement::Restricted)
        );
    }

    #[test]
    fn preview_datasets_require_prompt_before_cutoff() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 5, 27).unwrap();
        assert_eq!(
            api_key_requirement_for_download(
                Some(&DatasetAccess {
                    status: DatasetAccessStatus::Preview,
                    public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
                }),
                selected_date,
                today,
                false,
                1
            ),
            Some(ApiKeyRequirement::Preview {
                public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
            })
        );
    }

    #[test]
    fn preview_datasets_do_not_require_prompt_on_or_after_cutoff() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 5, 28).unwrap();
        assert_eq!(
            api_key_requirement_for_download(
                Some(&DatasetAccess {
                    status: DatasetAccessStatus::Preview,
                    public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
                }),
                selected_date,
                today,
                false,
                1
            ),
            None
        );
    }

    #[test]
    fn legacy_older_downloads_without_access_metadata_require_prompt() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 2).unwrap();
        assert_eq!(
            api_key_requirement_for_download(None, selected_date, today, false, 1),
            Some(ApiKeyRequirement::LegacyPreviewWindow)
        );
    }

    #[test]
    fn prompt_is_skipped_when_api_key_exists_or_no_download_is_needed() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        assert_eq!(
            api_key_requirement_for_download(
                Some(&DatasetAccess {
                    status: DatasetAccessStatus::Restricted,
                    public_cutoff_date: None,
                }),
                selected_date,
                today,
                true,
                1
            ),
            None
        );
        assert_eq!(
            api_key_requirement_for_download(
                Some(&DatasetAccess {
                    status: DatasetAccessStatus::Restricted,
                    public_cutoff_date: None,
                }),
                selected_date,
                today,
                false,
                0
            ),
            None
        );
    }
}
