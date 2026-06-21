use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::mpsc::{error::TryRecvError, unbounded_channel};

use crate::api::{CliAuthPollResponse, PolarisClient};
use crate::auth::{CredentialStore, KeychainCredentialStore};
use crate::config::{ApiKeySource, Config};
use crate::error::{Result, TickError};
use crate::layout::{Layout as AppLayout, LocalSnapshotEntry};
use crate::planner::{TimeWindow, build_sync_plan};
use crate::syncer::{acquire_sync_lock, execute_sync_with_progress};

use super::coverage::{
    api_key_requirement_for_download, browser_categories, build_day_coverages,
    format_byte_progress, group_local_snapshot_keys, select_initial_day, summarize_local_snapshots,
};
use super::model::{
    AccountIdentity, AccountLoginSession, AccountView, ActiveDaySync, ApiKeyPromptState,
    ApiKeyRequirement, BrowserCategory, DatasetView, DaySyncUpdate, FileManagerTarget,
    LocalDatasetSummary, RemoteDatasetEntry, RemoteTuiSeed, ViewMode,
};
use super::storage::{
    load_account_identity, load_bookmarks, open_in_file_manager, open_url, save_account_identity,
    save_bookmarks, snapshot_reveal_target,
};

const MOCK_REST_API_URL: &str = "https://api.polaris.supply";
const MIN_CLI_AUTH_POLL_INTERVAL_MS: u64 = 250;

#[derive(Debug)]
enum CliLoginUpdate {
    Approved {
        api_key: String,
        identity: AccountIdentity,
    },
    Error(String),
}

#[derive(Debug)]
enum AccountRefreshUpdate {
    Refreshed(AccountIdentity),
    Error(String),
}

#[derive(Debug)]
pub(crate) struct RemoteListTui {
    pub(crate) datasets: Vec<RemoteDatasetEntry>,
    pub(crate) filtered_indices: Vec<usize>,
    pub(crate) selected: usize,
    pub(crate) search: String,
    pub(crate) categories: Vec<BrowserCategory>,
    pub(crate) selected_category: usize,
    pub(crate) bookmarks: BTreeSet<String>,
    pub(crate) session_priority_bookmarks: BTreeSet<String>,
    pub(crate) local_summaries: BTreeMap<String, LocalDatasetSummary>,
    pub(crate) local_keys: BTreeMap<String, Vec<String>>,
    pub(crate) root: PathBuf,
    pub(crate) concurrency: usize,
    pub(crate) status_message: Option<String>,
    pub(crate) active_sync: Option<ActiveDaySync>,
    pub(crate) spinner_tick: usize,
    pub(crate) mode: ViewMode,
    pub(crate) account_view: AccountView,
    active_cli_login_updates: Option<tokio::sync::mpsc::UnboundedReceiver<CliLoginUpdate>>,
    active_account_refresh_updates:
        Option<tokio::sync::mpsc::UnboundedReceiver<AccountRefreshUpdate>>,
    pub(crate) api_key_prompt: Option<ApiKeyPromptState>,
}

impl RemoteListTui {
    pub(crate) fn new(
        datasets: Vec<RemoteDatasetEntry>,
        local_snapshots: Vec<LocalSnapshotEntry>,
        root: PathBuf,
        concurrency: usize,
        seed: RemoteTuiSeed,
    ) -> Self {
        let mut search = seed.search.unwrap_or_default();
        if search.is_empty() {
            if let Some(venue) = seed.venue {
                search = venue;
                if let Some(symbol) = seed.symbol {
                    search.push(':');
                    search.push_str(&symbol);
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
        let account_root = root.clone();
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
            account_view: Self::mock_account_view(account_root, concurrency),
            active_cli_login_updates: None,
            active_account_refresh_updates: None,
            api_key_prompt: None,
        };
        app.recompute_filter();
        app
    }

    fn mock_account_view(root: PathBuf, _concurrency: usize) -> AccountView {
        AccountView {
            api_key_present: false,
            api_key_source_label: "not configured".into(),
            base_url: MOCK_REST_API_URL.into(),
            root,
            active_login: None,
            identity: None,
        }
    }

    fn runtime_api_key_source_label(source: Option<ApiKeySource>) -> &'static str {
        match source {
            Some(ApiKeySource::Environment) => "POLARIS_API_KEY environment variable",
            Some(ApiKeySource::CredentialStore) => "stored credential",
            None => "not configured",
        }
    }

    fn account_view_from_config(config: &Config) -> AccountView {
        AccountView {
            api_key_present: config.api_key.is_some(),
            api_key_source_label: Self::runtime_api_key_source_label(config.api_key_source).into(),
            base_url: config.base_url.clone(),
            root: config.root.clone(),
            active_login: None,
            identity: None,
        }
    }

    pub(crate) fn apply_runtime_config(&mut self, config: &Config) {
        let active_login = self.account_view.active_login.clone();
        let identity = self.account_view.identity.clone().or_else(|| {
            config
                .api_key
                .as_ref()
                .and_then(|_| load_account_identity(&config.root).ok().flatten())
        });
        self.account_view = Self::account_view_from_config(config);
        self.account_view.active_login = active_login;
        self.account_view.identity = identity;
    }

    pub(crate) async fn hydrate_account_identity(&mut self, client: &PolarisClient) -> Result<()> {
        if !client.has_api_key() {
            return Ok(());
        }

        let identity = Self::fetch_account_identity(client).await?;
        save_account_identity(&self.account_view.root, &identity)?;
        self.account_view.identity = Some(identity);
        Ok(())
    }

    async fn fetch_account_identity(client: &PolarisClient) -> Result<AccountIdentity> {
        let account = client.fetch_account().await?;
        Ok(AccountIdentity {
            user_id: account.user_id,
            display_name: account.identity.display_name,
            email: account.identity.email,
            plan: Some(account.subscription.tier),
            wallet_address: account.identity.wallet_address,
            avatar_url: account.identity.avatar_url,
        })
    }

    pub(crate) fn open_pricing_page(&mut self) -> Result<()> {
        open_url("https://polaris.supply/pricing")?;
        self.status_message = Some("opened https://polaris.supply/pricing in your browser".into());
        Ok(())
    }

    pub(crate) fn refresh_account_details(&mut self, client: &PolarisClient) -> Result<()> {
        if !client.has_api_key() {
            self.status_message = Some("account refresh requires a stored API key".into());
            return Ok(());
        }

        let mut identity = self
            .account_view
            .identity
            .clone()
            .unwrap_or(AccountIdentity {
                user_id: String::new(),
                display_name: None,
                email: None,
                plan: None,
                wallet_address: None,
                avatar_url: None,
            });
        identity.display_name = None;
        identity.email = None;
        identity.plan = None;
        self.account_view.identity = Some(identity);

        let (tx, rx) = unbounded_channel();
        let client = client.clone();
        tokio::spawn(async move {
            match Self::fetch_account_identity(&client).await {
                Ok(identity) => {
                    let _ = tx.send(AccountRefreshUpdate::Refreshed(identity));
                }
                Err(err) => {
                    let _ = tx.send(AccountRefreshUpdate::Error(err.to_string()));
                }
            }
        });
        self.active_account_refresh_updates = Some(rx);
        Ok(())
    }

    pub(crate) fn recompute_filter(&mut self) {
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

    pub(crate) fn selected_dataset(&self) -> Option<&RemoteDatasetEntry> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|index| self.datasets.get(*index))
    }

    pub(crate) fn selected_category(&self) -> &BrowserCategory {
        self.categories
            .get(self.selected_category)
            .expect("browser categories should always include defaults")
    }

    pub(crate) fn category_display_labels(&self) -> Vec<String> {
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

    pub(crate) fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.selected + 1 < self.filtered_indices.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn cycle_category(&mut self, delta: isize) {
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
            ViewMode::Browser | ViewMode::Splash | ViewMode::Account => self
                .selected_dataset()
                .map(|dataset| dataset.dataset.as_str()),
        }
    }

    pub(crate) fn is_bookmarked(&self, dataset: &str) -> bool {
        self.bookmarks.contains(dataset)
    }

    pub(crate) fn toggle_current_bookmark(&mut self) -> Result<()> {
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

    pub(crate) fn dataset_view(&self) -> Option<&DatasetView> {
        match &self.mode {
            ViewMode::Dataset(view) => Some(view),
            ViewMode::Browser | ViewMode::Splash | ViewMode::Account => None,
        }
    }

    pub(crate) fn open_account_view(&mut self) {
        self.mode = ViewMode::Account;
    }

    pub(crate) async fn handle_account_shortcut(&mut self, client: &PolarisClient) -> Result<()> {
        if !matches!(self.mode, ViewMode::Account) {
            self.open_account_view();
            return Ok(());
        }

        if self.account_view.api_key_present {
            open_url("https://polaris.supply/account")?;
            self.status_message =
                Some("opened https://polaris.supply/account in your browser".into());
            return Ok(());
        }

        self.start_or_resume_cli_login(client).await
    }

    async fn start_or_resume_cli_login(&mut self, client: &PolarisClient) -> Result<()> {
        if let Some(active_login) = &self.account_view.active_login {
            open_url(&active_login.login_url)?;
            self.status_message = Some(format!(
                "reopened browser login; enter code {}",
                active_login.user_code
            ));
            return Ok(());
        }

        let start = client.start_cli_auth().await?;
        let login_url = start.login_url.clone();
        let user_code = start.user_code.clone();
        let expires_at = start.expires_at;
        let (tx, rx) = unbounded_channel();
        let poll_client = client.clone();
        let request_id = start.request_id;
        let poll_token = start.poll_token;
        let initial_interval_ms = start.interval_ms.max(MIN_CLI_AUTH_POLL_INTERVAL_MS);

        tokio::spawn(async move {
            let mut interval_ms = initial_interval_ms;
            loop {
                match poll_client.poll_cli_auth(&request_id, &poll_token).await {
                    Ok(CliAuthPollResponse::Pending {
                        interval_ms: next_interval_ms,
                        ..
                    }) => {
                        interval_ms = next_interval_ms.max(MIN_CLI_AUTH_POLL_INTERVAL_MS);
                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                    }
                    Ok(CliAuthPollResponse::Approved {
                        user_id,
                        display_name,
                        email,
                        wallet_address,
                        avatar_url,
                        api_key,
                        ..
                    }) => {
                        let _ = tx.send(CliLoginUpdate::Approved {
                            api_key,
                            identity: AccountIdentity {
                                user_id,
                                display_name,
                                email,
                                plan: None,
                                wallet_address,
                                avatar_url,
                            },
                        });
                        return;
                    }
                    Ok(CliAuthPollResponse::Consumed) => {
                        let _ = tx.send(CliLoginUpdate::Error(
                            "login session was already consumed".into(),
                        ));
                        return;
                    }
                    Ok(CliAuthPollResponse::Expired) => {
                        let _ = tx.send(CliLoginUpdate::Error("login session expired".into()));
                        return;
                    }
                    Err(err) if err.retryable() => {
                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                    }
                    Err(err) => {
                        let _ = tx.send(CliLoginUpdate::Error(err.to_string()));
                        return;
                    }
                }
            }
        });

        self.account_view.active_login = Some(AccountLoginSession {
            user_code: user_code.clone(),
            login_url: login_url.clone(),
            expires_at,
        });
        self.active_cli_login_updates = Some(rx);
        self.status_message = Some(format!(
            "waiting for browser approval; enter code {}",
            user_code
        ));

        if let Err(err) = open_url(&login_url) {
            self.status_message = Some(format!(
                "open {} manually and enter code {} ({err})",
                login_url, user_code
            ));
        }
        Ok(())
    }

    pub(crate) async fn open_selected_dataset(&mut self, client: &PolarisClient) -> Result<()> {
        let Some(dataset) = self.selected_dataset().cloned() else {
            return Ok(());
        };

        let (remote_snapshots, _) = client
            .list_snapshots(&dataset.venue, &dataset.symbol, dataset.start, dataset.end)
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

    pub(crate) fn reveal_selected_day_snapshot(&mut self) -> Result<()> {
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

    pub(crate) async fn sync_selected_day(&mut self, client: &PolarisClient) -> Result<()> {
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
            &dataset.venue,
            &dataset.symbol,
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

    pub(crate) fn api_key_requirement_for_selected_day(
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

    pub(crate) fn open_api_key_prompt(&mut self, requirement: ApiKeyRequirement) {
        if self.api_key_prompt.is_none() {
            self.api_key_prompt = Some(ApiKeyPromptState {
                input: String::new(),
                error_message: None,
                access_message: requirement.message(),
            });
        }
    }

    pub(crate) fn close_api_key_prompt(&mut self) {
        self.api_key_prompt = None;
    }

    pub(crate) fn push_api_key_prompt_char(&mut self, c: char) {
        if let Some(prompt) = &mut self.api_key_prompt {
            prompt.input.push(c);
            prompt.error_message = None;
        }
    }

    pub(crate) fn pop_api_key_prompt_char(&mut self) {
        if let Some(prompt) = &mut self.api_key_prompt {
            prompt.input.pop();
            prompt.error_message = None;
        }
    }

    fn set_api_key_prompt_error(&mut self, message: String) {
        if let Some(prompt) = &mut self.api_key_prompt {
            prompt.error_message = Some(message);
        }
    }

    fn apply_runtime_client_config(
        &mut self,
        client: &mut PolarisClient,
        config: &Config,
    ) -> Result<()> {
        self.apply_runtime_config(config);
        *client = PolarisClient::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.timeout,
        )?;
        Ok(())
    }

    fn store_api_key_and_load_config<S, F>(
        store: &S,
        api_key: &str,
        mut load_config: F,
    ) -> Result<Config>
    where
        S: CredentialStore,
        F: FnMut() -> Result<Config>,
    {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(TickError::InvalidArgument("API key cannot be empty".into()));
        }

        store.set_api_key(api_key)?;
        load_config()
    }

    pub(crate) async fn submit_api_key_prompt(&mut self, client: &mut PolarisClient) -> Result<()> {
        if self.api_key_prompt.is_none() {
            return Ok(());
        }

        let api_key = self
            .api_key_prompt
            .as_ref()
            .map(|prompt| prompt.input.trim().to_string())
            .unwrap_or_default();
        if api_key.is_empty() {
            self.set_api_key_prompt_error("API key cannot be empty".into());
            return Ok(());
        }

        let store = match KeychainCredentialStore::new() {
            Ok(store) => store,
            Err(err) => {
                self.set_api_key_prompt_error(err.to_string());
                return Ok(());
            }
        };
        let config = match Self::store_api_key_and_load_config(&store, &api_key, Config::from_env) {
            Ok(config) => config,
            Err(err) => {
                self.set_api_key_prompt_error(err.to_string());
                return Ok(());
            }
        };
        if let Err(err) = self.apply_runtime_client_config(client, &config) {
            self.set_api_key_prompt_error(err.to_string());
            return Ok(());
        }

        self.close_api_key_prompt();
        self.sync_selected_day(client).await
    }

    fn apply_session_api_key(
        &mut self,
        client: &mut PolarisClient,
        api_key: String,
        source_label: &str,
    ) -> Result<()> {
        *client = PolarisClient::new(
            self.account_view.base_url.clone(),
            Some(api_key),
            Duration::from_secs(60),
        )?;
        self.account_view.api_key_present = true;
        self.account_view.api_key_source_label = source_label.into();
        Ok(())
    }

    pub(crate) async fn pump_cli_login_updates(
        &mut self,
        client: &mut PolarisClient,
    ) -> Result<()> {
        let Some(receiver) = self.active_cli_login_updates.as_mut() else {
            return Ok(());
        };

        let update = receiver.try_recv();
        match update {
            Ok(CliLoginUpdate::Approved { api_key, identity }) => {
                self.account_view.active_login = None;
                self.account_view.identity = Some(identity.clone());
                self.active_cli_login_updates = None;
                let identity_persist_result =
                    save_account_identity(&self.account_view.root, &identity);

                match KeychainCredentialStore::new().and_then(|store| {
                    Self::store_api_key_and_load_config(&store, &api_key, Config::from_env)
                }) {
                    Ok(config) => {
                        self.apply_runtime_client_config(client, &config)?;
                        if let Err(err) = self.hydrate_account_identity(client).await {
                            self.status_message = Some(format!(
                                "signed in, but failed to refresh account details: {err}"
                            ));
                            return Ok(());
                        }
                        let display_name = identity
                            .display_name
                            .as_deref()
                            .or(identity.email.as_deref())
                            .unwrap_or(identity.user_id.as_str());
                        self.status_message = Some(match identity_persist_result {
                            Ok(()) => format!("signed in as {display_name}; API key saved"),
                            Err(err) => format!(
                                "signed in as {display_name}; API key saved, but failed to persist account details: {err}"
                            ),
                        });
                    }
                    Err(err) => {
                        self.apply_session_api_key(
                            client,
                            api_key,
                            "current session only (credential store failed)",
                        )?;
                        let _ = self.hydrate_account_identity(client).await;
                        self.status_message = Some(match identity_persist_result {
                            Ok(()) => format!("signed in, but failed to persist API key: {err}"),
                            Err(identity_err) => format!(
                                "signed in, but failed to persist API key: {err}; also failed to persist account details: {identity_err}"
                            ),
                        });
                    }
                }
            }
            Ok(CliLoginUpdate::Error(message)) => {
                self.account_view.active_login = None;
                self.active_cli_login_updates = None;
                self.status_message = Some(message);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.account_view.active_login = None;
                self.active_cli_login_updates = None;
                self.status_message = Some("browser login stopped unexpectedly".into());
            }
        }

        Ok(())
    }

    pub(crate) fn pump_account_refresh_updates(&mut self) -> Result<()> {
        let Some(receiver) = self.active_account_refresh_updates.as_mut() else {
            return Ok(());
        };

        match receiver.try_recv() {
            Ok(AccountRefreshUpdate::Refreshed(identity)) => {
                save_account_identity(&self.account_view.root, &identity)?;
                self.account_view.identity = Some(identity);
                self.active_account_refresh_updates = None;
            }
            Ok(AccountRefreshUpdate::Error(message)) => {
                self.active_account_refresh_updates = None;
                self.status_message = Some(format!("error: {message}"));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.active_account_refresh_updates = None;
                self.status_message = Some("account refresh stopped unexpectedly".into());
            }
        }

        Ok(())
    }

    pub(crate) async fn pump_sync_updates(&mut self, client: &PolarisClient) -> Result<()> {
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
            let progress = format_byte_progress(sync.download_bytes, sync.download_total_bytes);
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
            .list_snapshots(&dataset.venue, &dataset.symbol, dataset.start, dataset.end)
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
