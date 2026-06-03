use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde::Serialize;
use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError, unbounded_channel};

use crate::api::{PolarisClient, SnapshotEntry};
use crate::auth::{CredentialStore, KeychainCredentialStore};
use crate::config::Config;
use crate::error::{Result, TickError};
use crate::layout::{LocalDailyArtifactEntry, LocalSnapshotEntry, infer_snapshot_date_from_key};
use crate::materialize::materialize_range_days;
use crate::planner::{TimeWindow, build_sync_plan};
use crate::syncer::{SyncProgressEvent, acquire_sync_lock, execute_sync_with_progress};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteDatasetEntry {
    pub exchange: String,
    pub asset: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: Option<String>,
    pub dataset: String,
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
    daily_artifact_exists: bool,
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
    phase: SyncPhase,
    deferred_update: Option<DaySyncUpdate>,
    receiver: UnboundedReceiver<DaySyncUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncPhase {
    Downloading,
    Materializing,
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
    Materializing,
    Finished {
        downloaded: usize,
        failed: usize,
        materialized_days: usize,
    },
    Error(String),
}

#[derive(Debug)]
enum ViewMode {
    Browser,
    Dataset(DatasetView),
}

#[derive(Debug)]
struct RemoteListTui {
    datasets: Vec<RemoteDatasetEntry>,
    filtered_indices: Vec<usize>,
    selected: usize,
    search: String,
    local_summaries: BTreeMap<String, LocalDatasetSummary>,
    local_keys: BTreeMap<String, Vec<String>>,
    daily_artifacts: BTreeSet<String>,
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
        local_daily_artifacts: Vec<LocalDailyArtifactEntry>,
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

        let local_summaries = summarize_local_snapshots(&local_snapshots);
        let local_keys = group_local_snapshot_keys(local_snapshots);
        let daily_artifacts = group_local_daily_artifacts(local_daily_artifacts);
        let mut app = Self {
            datasets,
            filtered_indices: Vec::new(),
            selected: 0,
            search,
            local_summaries,
            local_keys,
            daily_artifacts,
            root,
            concurrency,
            status_message: None,
            active_sync: None,
            spinner_tick: 0,
            mode: ViewMode::Browser,
            api_key_prompt: None,
        };
        app.recompute_filter();
        app
    }

    fn recompute_filter(&mut self) {
        let needle = self.search.to_ascii_lowercase();
        self.filtered_indices = self
            .datasets
            .iter()
            .enumerate()
            .filter_map(|(index, dataset)| {
                if needle.is_empty() || dataset.dataset.to_ascii_lowercase().contains(&needle) {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        if self.filtered_indices.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len() - 1;
        }
    }

    fn selected_dataset(&self) -> Option<&RemoteDatasetEntry> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|index| self.datasets.get(*index))
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

    fn dataset_view(&self) -> Option<&DatasetView> {
        match &self.mode {
            ViewMode::Dataset(view) => Some(view),
            ViewMode::Browser => None,
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
            &dataset.dataset,
            &self.daily_artifacts,
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
        let local_daily_artifacts = layout.list_local_daily_artifacts()?;
        self.local_summaries = summarize_local_snapshots(&local_snapshots);
        self.local_keys = group_local_snapshot_keys(local_snapshots);
        self.daily_artifacts = group_local_daily_artifacts(local_daily_artifacts);
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
        let exchange = dataset.exchange.clone();
        let asset = dataset.asset.clone();
        let from_date = plan.effective_range.from.date_naive();
        let to_date = plan.effective_range.to.date_naive();
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
            let _ = tx.send(DaySyncUpdate::Materializing);
            match materialize_range_days(&client, &layout, &exchange, &asset, from_date, to_date)
                .await
            {
                Ok(materialization) => {
                    let _ = tx.send(DaySyncUpdate::Finished {
                        downloaded: execution.downloaded_total(),
                        failed: execution.failed_total(),
                        materialized_days: materialization.built_total,
                    });
                }
                Err(err) => {
                    let _ = tx.send(DaySyncUpdate::Error(err.to_string()));
                }
            }
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
            phase: SyncPhase::Downloading,
            deferred_update: None,
            receiver: rx,
        });
        self.status_message = Some(format!("syncing {}", selected_date));
        Ok(())
    }

    fn should_prompt_for_api_key(&self, today: NaiveDate, has_api_key: bool) -> bool {
        let Some(view) = self.dataset_view() else {
            return false;
        };
        let day = view.selected_coverage();
        requires_api_key_for_download(day.date, today, has_api_key, day.missing_keys.len())
    }

    fn open_api_key_prompt(&mut self) {
        if self.api_key_prompt.is_none() {
            self.api_key_prompt = Some(ApiKeyPromptState::default());
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
                    Ok(DaySyncUpdate::Materializing) => {
                        if saw_progress_update {
                            sync.deferred_update = Some(DaySyncUpdate::Materializing);
                            break;
                        }
                        sync.phase = SyncPhase::Materializing;
                        break;
                    }
                    Ok(DaySyncUpdate::Finished {
                        downloaded,
                        failed,
                        materialized_days,
                    }) => {
                        if saw_progress_update {
                            sync.deferred_update = Some(DaySyncUpdate::Finished {
                                downloaded,
                                failed,
                                materialized_days,
                            });
                            break;
                        }
                        finished = Some((
                            sync.dataset.clone(),
                            sync.date,
                            format!(
                                "synced {} snapshot(s), failed {}, materialized {} day(s)",
                                downloaded, failed, materialized_days
                            ),
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
            self.status_message = Some(match sync.phase {
                SyncPhase::Downloading => {
                    let progress =
                        format_byte_progress(sync.download_bytes, sync.download_total_bytes);
                    if progress.is_empty() {
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
                    }
                }
                SyncPhase::Materializing => format!("materializing {}", sync.date),
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
            &dataset.dataset,
            &self.daily_artifacts,
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
    local_daily_artifacts: Vec<LocalDailyArtifactEntry>,
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
            local_daily_artifacts,
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
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => match app.mode {
                        ViewMode::Browser => return Ok(()),
                        ViewMode::Dataset(_) => app.mode = ViewMode::Browser,
                    },
                    KeyCode::Enter => {
                        if matches!(app.mode, ViewMode::Browser) {
                            if let Err(err) = app.open_selected_dataset(&client).await {
                                app.status_message = Some(format!("error: {err}"));
                            }
                        } else if app.should_prompt_for_api_key(
                            Utc::now().date_naive(),
                            client.has_api_key(),
                        ) {
                            app.open_api_key_prompt();
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
                        }
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        if let ViewMode::Dataset(view) = &mut app.mode {
                            view.move_selection(1);
                        }
                    }
                    KeyCode::Backspace => {
                        if matches!(app.mode, ViewMode::Browser) {
                            app.search.pop();
                            app.recompute_filter();
                        }
                    }
                    KeyCode::Char(c) => {
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

fn render(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    match app.dataset_view() {
        Some(view) => render_dataset_view(
            frame,
            view,
            app.status_message.as_deref(),
            app.active_sync.as_ref(),
            app.spinner_tick,
        ),
        None => render_browser(frame, app),
    }

    if let Some(prompt) = &app.api_key_prompt {
        render_api_key_prompt(frame, prompt);
    }
}

fn render_browser(frame: &mut ratatui::Frame<'_>, app: &RemoteListTui) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.area());

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(areas[1]);

    let search = Paragraph::new(app.search.clone()).block(
        Block::default()
            .title("Search exchange:asset")
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
                ListItem::new(Line::from(vec![Span::styled(
                    dataset.dataset.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )]))
            })
            .collect()
    };

    let mut state = ListState::default()
        .with_selected((!app.filtered_indices.is_empty()).then_some(app.selected));
    let list = List::new(items)
        .block(Block::default().title("Datasets").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, content[0], &mut state);

    let details = render_browser_details(app);
    frame.render_widget(Clear, content[1]);
    frame.render_widget(details, content[1]);
}

fn render_browser_details(app: &RemoteListTui) -> Paragraph<'static> {
    let lines = if let Some(dataset) = app.selected_dataset() {
        let local = app.local_summaries.get(&dataset.dataset);
        let local_count = local.map(|summary| summary.snapshot_count).unwrap_or(0);
        let local_first = local
            .and_then(|summary| summary.first_start)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".into());
        let local_last = local
            .and_then(|summary| summary.last_end)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".into());

        vec![
            Line::from(vec![Span::styled(
                dataset.dataset.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )]),
            Line::from(format!("exchange: {}", dataset.exchange)),
            Line::from(format!("asset: {}", dataset.asset)),
            Line::from(format!("remote start: {}", dataset.start)),
            Line::from(format!("remote end: {}", dataset.end)),
            Line::from(format!(
                "source: {}",
                dataset.source.clone().unwrap_or_else(|| "-".into())
            )),
            Line::from(""),
            Line::from(format!("local snapshots: {}", local_count)),
            Line::from(format!("local first: {}", local_first)),
            Line::from(format!("local last: {}", local_last)),
            Line::from(""),
            Line::from("Keys"),
            Line::from("Type to search"),
            Line::from("Up/Down move selection"),
            Line::from("Enter inspect snapshots"),
            Line::from("q or Esc quit"),
        ]
    } else {
        vec![Line::from("No dataset selected")]
    };

    let mut lines = lines;
    if let Some(status) = &app.status_message {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("status: {status}")));
    }

    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .title("Dataset Details")
            .borders(Borders::ALL),
    )
}

fn render_dataset_view(
    frame: &mut ratatui::Frame<'_>,
    view: &DatasetView,
    status_message: Option<&str>,
    active_sync: Option<&ActiveDaySync>,
    spinner_tick: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(8),
            Constraint::Length(12),
        ])
        .split(frame.area());

    let mut summary_lines = vec![
        Line::from(vec![Span::styled(
            view.dataset.dataset.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!("data starting from {}", view.dataset.start)),
        Line::from("Enter sync day, Left/Right move day, Up/Down move week"),
        Line::from("Esc back, q quit"),
    ];
    if let Some(status) = status_message {
        summary_lines.push(Line::from(format!("status: {status}")));
    }

    let summary = Paragraph::new(summary_lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().title("Dataset View").borders(Borders::ALL));
    frame.render_widget(summary, areas[0]);

    frame.render_widget(render_day_grid(view, active_sync, spinner_tick), areas[1]);
    frame.render_widget(render_selected_day_summary(view, active_sync), areas[2]);
}

fn render_day_grid(
    view: &DatasetView,
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

    Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .title("Daily Coverage")
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
) -> Paragraph<'static> {
    let day = view.selected_coverage();
    let (remote_total, local_total, missing_total, state) =
        sync_adjusted_day_totals(view, day, active_sync);
    let remote_window = format_snapshot_window(&day.remote_keys);
    let local_window = format_snapshot_window(&day.local_keys);
    let completion_bar = render_completion_bar(local_total, remote_total, 24);
    let byte_progress = active_sync
        .filter(|sync| sync.dataset == view.dataset.dataset && sync.date == day.date)
        .map(|sync| format_byte_progress(sync.download_bytes, sync.download_total_bytes))
        .filter(|progress| !progress.is_empty())
        .unwrap_or_else(|| "-".to_string());

    Paragraph::new(vec![
        Line::from(vec![Span::styled(
            day.date.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!("state: {}", state)),
        Line::from(format!("coverage: {}", completion_bar)),
        Line::from(format!("remote snapshots: {}", remote_total)),
        Line::from(format!("local snapshots: {}", local_total)),
        Line::from(format!("missing snapshots: {}", missing_total)),
        Line::from(format!("download bytes: {}", byte_progress)),
        Line::from(format!("remote window: {}", remote_window)),
        Line::from(format!("local window: {}", local_window)),
        Line::from(format!(
            "daily artifact: {}",
            if day.daily_artifact_exists {
                "present"
            } else {
                "absent"
            }
        )),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().title("Day Details").borders(Borders::ALL))
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
            "Older than 7 days requires a Polaris API key.",
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
    dataset: &str,
    daily_artifacts: &BTreeSet<String>,
) -> Vec<DayCoverage> {
    let mut days = Vec::new();
    let mut current = start_date;
    while current <= end_date {
        days.push(DayCoverage {
            date: current,
            remote_keys: Vec::new(),
            local_keys: Vec::new(),
            missing_keys: Vec::new(),
            daily_artifact_exists: daily_artifacts.contains(&format!("{dataset}:{current}")),
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

fn group_local_daily_artifacts(entries: Vec<LocalDailyArtifactEntry>) -> BTreeSet<String> {
    entries
        .into_iter()
        .map(|entry| format!("{}:{}:{}", entry.exchange, entry.asset, entry.date))
        .collect()
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
            let state = match sync.phase {
                SyncPhase::Downloading => "syncing",
                SyncPhase::Materializing => "materializing",
            };
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

fn format_snapshot_window(keys: &[String]) -> String {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for key in keys {
        if let Some((start, end)) = parse_snapshot_times_from_key(key) {
            starts.push(start);
            ends.push(end);
        }
    }
    starts.sort();
    ends.sort();

    match (starts.first(), ends.last()) {
        (Some(start), Some(end)) => {
            format!("{} -> {}", start.format("%H:%M:%S"), end.format("%H:%M:%S"))
        }
        _ => "-".to_string(),
    }
}

fn parse_snapshot_times_from_key(key: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let filename = key.rsplit('/').next()?;
    let start = filename
        .split("_s")
        .nth(1)
        .and_then(|value| value.split("_e").next())
        .and_then(parse_snapshot_timestamp)?;
    let end = filename
        .split("_e")
        .nth(1)
        .and_then(|value| value.split('.').next())
        .and_then(parse_snapshot_timestamp)?;
    Some((start, end))
}

fn parse_snapshot_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%SZ").ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
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

fn requires_api_key_for_download(
    selected_date: NaiveDate,
    today: NaiveDate,
    has_api_key: bool,
    missing_snapshot_count: usize,
) -> bool {
    !has_api_key && missing_snapshot_count > 0 && selected_date < today - ChronoDuration::days(7)
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
        ActiveDaySync, DatasetView, DaySyncUpdate, RemoteDatasetEntry, RemoteListTui,
        RemoteTuiSeed, SyncPhase, build_day_coverages, diff_missing_snapshot_keys,
        requires_api_key_for_download,
    };
    use crate::api::{PolarisClient, SnapshotEntry};
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
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                exchange: "binance".into(),
                asset: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                source: Some("manifest".into()),
                dataset: "binance:ETHUSDT".into(),
            },
        ];

        let app = RemoteListTui::new(
            datasets,
            Vec::<LocalSnapshotEntry>::new(),
            Vec::new(),
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
            "aster:BTCUSDT",
            &BTreeSet::new(),
        );

        assert_eq!(days[0].missing_keys.len(), 0);
        assert_eq!(days[1].missing_keys.len(), 0);
        assert_eq!(days[2].missing_keys.len(), 1);
        assert_eq!(days[3].remote_keys.len(), 0);
    }

    #[tokio::test]
    async fn sync_updates_do_not_skip_progress_or_materializing_frames() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let dataset = RemoteDatasetEntry {
            exchange: "aster".into(),
            asset: "ASTERUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 1, 23, 59, 59).unwrap(),
            source: Some("manifest".into()),
            dataset: "aster:ASTERUSDT".into(),
        };
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let mut app = RemoteListTui::new(
            vec![dataset.clone()],
            Vec::<LocalSnapshotEntry>::new(),
            Vec::new(),
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
                &dataset.dataset,
                &BTreeSet::new(),
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
        tx.send(DaySyncUpdate::Materializing)
            .expect("materializing");
        tx.send(DaySyncUpdate::Finished {
            downloaded: 1,
            failed: 0,
            materialized_days: 1,
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
            phase: SyncPhase::Downloading,
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
        assert_eq!(sync.phase, SyncPhase::Downloading);
        assert!(matches!(
            sync.deferred_update,
            Some(DaySyncUpdate::Materializing)
        ));
        assert_eq!(
            app.status_message.as_deref(),
            Some("syncing 2026-06-01 (1/1, 2.00 KiB / 2.00 KiB)")
        );

        app.pump_sync_updates(&client).await.expect("second pump");
        let sync = app.active_sync.as_ref().expect("sync still active");
        assert_eq!(sync.phase, SyncPhase::Materializing);
        assert!(sync.deferred_update.is_none());
        assert_eq!(
            app.status_message.as_deref(),
            Some("materializing 2026-06-01")
        );

        app.pump_sync_updates(&client).await.expect("third pump");
        assert_eq!(
            app.status_message.as_deref(),
            Some("synced 1 snapshot(s), failed 0, materialized 1 day(s)")
        );
        assert!(app.active_sync.is_none());
    }

    #[test]
    fn older_downloads_without_api_key_require_prompt() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 2).unwrap();
        assert!(requires_api_key_for_download(
            selected_date,
            today,
            false,
            1
        ));
    }

    #[test]
    fn seven_day_old_download_does_not_require_prompt() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 3).unwrap();
        assert!(!requires_api_key_for_download(
            selected_date,
            today,
            false,
            1
        ));
    }

    #[test]
    fn prompt_is_skipped_when_api_key_exists_or_no_download_is_needed() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let selected_date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        assert!(!requires_api_key_for_download(
            selected_date,
            today,
            true,
            1
        ));
        assert!(!requires_api_key_for_download(
            selected_date,
            today,
            false,
            0
        ));
    }
}
