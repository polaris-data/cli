use std::env;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use clap::Parser;
use rpassword::prompt_password;
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::api::{CatalogExchange, PolarisClient};
use crate::auth::{CredentialStore, KeychainCredentialStore};
use crate::cli::{
    AccountCommand, AccountSubcommand, Cli, Command, DatasetArgs, ListCommand, ListSubcommand,
    LocalListArgs, RemoteListArgs, ResetArgs, SyncArgs, UpdateArgs,
};
use crate::config::{ApiKeySource, Config};
use crate::error::{Result, TickError};
use crate::layout::{Layout, LocalSnapshotEntry};
use crate::materialize::{MaterializeExecution, materialize_range_days};
use crate::planner::{SyncPlan, TimeWindow, build_sync_plan};
use crate::syncer::{SyncExecution, acquire_sync_lock, execute_sync, layout_for_root};
use crate::tui::{RemoteDatasetEntry, RemoteTuiSeed, can_render_tui, run_remote_list_tui};

const UPDATE_INSTALLER_URL: &str =
    "https://raw.githubusercontent.com/spectrum-ec/tick/main/install.sh";

pub async fn main_entry() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(err.exit_code())
        }
    }
}

pub async fn run(cli: Cli) -> Result<u8> {
    match cli.command {
        Some(Command::Account(args)) => run_account(args),
        Some(Command::List(args)) => {
            let config = Config::from_env()?;
            run_list(&config, args).await
        }
        Some(Command::Reset(args)) => {
            let config = Config::from_env()?;
            run_reset(&config, args).await
        }
        Some(Command::Sync(args)) => {
            let config = Config::from_env()?;
            let client = PolarisClient::new(
                config.base_url.clone(),
                config.api_key.clone(),
                config.timeout,
            )?;
            run_sync(&config, &client, args).await
        }
        Some(Command::Update(args)) => run_update(args).await,
        None => {
            let config = Config::from_env()?;
            run_browser(&config).await
        }
    }
}

fn run_account(args: AccountCommand) -> Result<u8> {
    match args.subcommand {
        AccountSubcommand::SetKey => run_account_set_key(),
        AccountSubcommand::Status => run_account_status(),
    }
}

fn run_account_set_key() -> Result<u8> {
    let api_key = prompt_password("Polaris API key: ")
        .context("failed to read API key from terminal")
        .map_err(TickError::Other)?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(TickError::InvalidArgument("API key cannot be empty".into()));
    }

    let store = KeychainCredentialStore::new()?;
    store.set_api_key(&api_key)?;
    println!("Stored Polaris API key in the OS credential store.");
    Ok(0)
}

fn run_account_status() -> Result<u8> {
    let config = Config::from_env()?;
    let status = match config.api_key_source {
        Some(ApiKeySource::Environment) => "configured via POLARIS_API_KEY",
        Some(ApiKeySource::CredentialStore) => "configured via OS credential store",
        None => "not configured",
    };
    println!("Polaris account status: {status}");
    println!("Base URL: {}", config.base_url);
    Ok(0)
}

async fn run_update(args: UpdateArgs) -> Result<u8> {
    let temp_dir = create_update_temp_dir()?;
    let installer_path = temp_dir.path().join("install.sh");
    download_update_installer(&installer_path).await?;

    let inferred_install_dir = match args.install_dir {
        Some(path) => Some(path),
        None => infer_current_install_dir()?,
    };
    let version = args.version;

    let status = tokio::task::spawn_blocking(move || {
        run_update_installer(
            &installer_path,
            version.as_deref(),
            inferred_install_dir.as_deref(),
        )
    })
    .await
    .context("update task failed")
    .map_err(TickError::Other)??;

    if !status.success() {
        return Err(TickError::Other(anyhow!(
            "tick update failed with status {status}"
        )));
    }

    Ok(0)
}

async fn download_update_installer(path: &Path) -> Result<()> {
    let script = reqwest::get(UPDATE_INSTALLER_URL)
        .await
        .context("failed to download install.sh")
        .map_err(TickError::Other)?
        .error_for_status()
        .context("failed to download install.sh")
        .map_err(TickError::Other)?
        .bytes()
        .await
        .context("failed to read install.sh")
        .map_err(TickError::Other)?;

    tokio::fs::write(path, &script)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
        .map_err(TickError::Other)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(path, permissions)
            .await
            .with_context(|| format!("failed to mark {} executable", path.display()))
            .map_err(TickError::Other)?;
    }

    Ok(())
}

fn run_update_installer(
    installer_path: &Path,
    version: Option<&str>,
    install_dir: Option<&Path>,
) -> Result<std::process::ExitStatus> {
    let mut command = std::process::Command::new("bash");
    command
        .arg(installer_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(version) = version {
        command.arg("--version").arg(version);
    }
    if let Some(install_dir) = install_dir {
        command.arg("--install-dir").arg(install_dir);
    }

    command
        .status()
        .context("failed to execute install.sh")
        .map_err(TickError::Other)
}

fn infer_current_install_dir() -> Result<Option<PathBuf>> {
    let current_exe = env::current_exe()
        .context("failed to determine current executable path")
        .map_err(TickError::Other)?;
    Ok(infer_install_dir_from_executable(&current_exe))
}

fn infer_install_dir_from_executable(executable: &Path) -> Option<PathBuf> {
    if executable.file_name()? != "tick" {
        return None;
    }

    let install_dir = executable.parent()?;
    if looks_like_cargo_target_dir(install_dir) {
        return None;
    }

    Some(install_dir.to_path_buf())
}

fn looks_like_cargo_target_dir(path: &Path) -> bool {
    let mut saw_target = false;
    for component in path.components() {
        let part = component.as_os_str();
        if part == "target" {
            saw_target = true;
            continue;
        }
        if saw_target && (part == "debug" || part == "release") {
            return true;
        }
    }
    false
}

struct UpdateTempDir {
    path: PathBuf,
}

impl UpdateTempDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UpdateTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn create_update_temp_dir() -> Result<UpdateTempDir> {
    let base = env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")
        .map_err(TickError::Other)?
        .as_nanos();

    for attempt in 0..32 {
        let path = base.join(format!(
            "tick-update-{}-{timestamp}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(UpdateTempDir { path }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(TickError::Other(anyhow!(err).context(format!(
                    "failed to create temporary update directory under {}",
                    base.display()
                ))));
            }
        }
    }

    Err(TickError::Other(anyhow!(
        "failed to allocate temporary update directory"
    )))
}

async fn run_browser(config: &Config) -> Result<u8> {
    let client = PolarisClient::new(
        config.base_url.clone(),
        config.api_key.clone(),
        config.timeout,
    )?;
    let args = RemoteListArgs {
        exchange: None,
        asset: None,
        search: None,
        limit: usize::MAX,
        json: false,
    };
    run_list_remote(config, &client, args, true).await
}

async fn run_list(config: &Config, args: ListCommand) -> Result<u8> {
    match args.subcommand {
        Some(ListSubcommand::Local(local)) => run_list_local(config, local),
        None => {
            let client = PolarisClient::new(
                config.base_url.clone(),
                config.api_key.clone(),
                config.timeout,
            )?;
            run_list_remote(config, &client, args.remote, false).await
        }
    }
}

async fn run_list_remote(
    config: &Config,
    client: &PolarisClient,
    args: RemoteListArgs,
    prefer_tui: bool,
) -> Result<u8> {
    if args.limit == 0 {
        return Err(TickError::InvalidArgument(
            "--limit must be greater than zero".into(),
        ));
    }
    let catalog = client
        .fetch_catalog(args.exchange.as_deref(), args.asset.as_deref())
        .await?;
    let filters = RemoteListFilters::from_args(&args);
    let entries = filter_remote_catalog(catalog.exchanges, &filters, args.limit);
    if args.json || !prefer_tui || !can_render_tui() {
        let output = RemoteListOutput::from_entries(filters, entries);
        emit_output(args.json, &output)?;
    } else {
        let local_snapshots = Layout::new(config.root.clone()).list_local_snapshots()?;
        let local_daily_artifacts =
            Layout::new(config.root.clone()).list_local_daily_artifacts()?;
        run_remote_list_tui(
            client.clone(),
            entries,
            local_snapshots,
            local_daily_artifacts,
            config.root.clone(),
            config.concurrency,
            RemoteTuiSeed {
                exchange: args.exchange.clone(),
                asset: args.asset.clone(),
                search: args.search.clone(),
            },
        )
        .await?;
    }
    Ok(0)
}

fn run_list_local(config: &Config, args: LocalListArgs) -> Result<u8> {
    let layout = Layout::new(config.root.clone());
    let entries = layout.list_local_snapshots()?;
    let filters = LocalListFilters::from_args(&args);
    let entries = filter_local_list_entries(entries, &filters);
    let output =
        LocalListOutput::from_entries(layout.root().display().to_string(), filters, entries);
    emit_output(args.json, &output)?;
    Ok(0)
}

async fn run_reset(config: &Config, args: ResetArgs) -> Result<u8> {
    let layout = layout_for_root(config.root.clone());
    let _guard = acquire_sync_lock(&layout)?;

    let snapshot_total = layout.list_local_snapshots()?.len();
    let daily_artifact_total = layout.list_local_daily_artifacts()?.len();

    let candidate_roots = vec![
        layout.data_root(),
        layout.daily_root(),
        layout.tmp_root(),
        layout.cache_root(),
    ];

    let mut removed_roots = Vec::new();
    for root in candidate_roots {
        if tokio::fs::metadata(&root).await.is_ok() {
            tokio::fs::remove_dir_all(&root)
                .await
                .with_context(|| format!("failed to remove {}", root.display()))
                .map_err(TickError::Other)?;
            removed_roots.push(root.display().to_string());
        }
    }

    let output = ResetOutput {
        command: "reset",
        root: layout.root().display().to_string(),
        snapshot_total,
        daily_artifact_total,
        removed_roots,
    };
    emit_output(args.json, &output)?;
    Ok(0)
}

async fn run_sync(config: &Config, client: &PolarisClient, args: SyncArgs) -> Result<u8> {
    let layout = layout_for_root(config.root.clone());
    let _guard = acquire_sync_lock(&layout)?;

    let requested_range = parse_requested_range(&args.dataset)?;
    let plan = build_sync_plan(
        client,
        config,
        &args.dataset.exchange,
        &args.dataset.asset,
        requested_range,
    )
    .await?;

    let concurrency = args.concurrency.unwrap_or(config.concurrency);
    if concurrency == 0 {
        return Err(TickError::InvalidArgument(
            "--concurrency must be greater than zero".into(),
        ));
    }

    let execution = execute_sync(client, &plan, concurrency).await;
    let materialization = materialize_range_days(
        client,
        &layout,
        &plan.exchange,
        &plan.asset,
        plan.effective_range.from.date_naive(),
        plan.effective_range.to.date_naive(),
    )
    .await?;
    let output = SyncOutput::from_parts(&plan, execution, materialization);
    let exit_code = if output.failed_total > 0 { 1 } else { 0 };
    emit_output(args.dataset.json, &output)?;
    Ok(exit_code)
}

fn parse_requested_range(dataset: &DatasetArgs) -> Result<TimeWindow> {
    let from = parse_datetime(&dataset.from, "--from")?;
    let to = parse_datetime(&dataset.to, "--to")?;
    if from > to {
        return Err(TickError::InvalidArgument(
            "--from must be less than or equal to --to".into(),
        ));
    }
    Ok(TimeWindow { from, to })
}

fn parse_datetime(raw: &str, flag: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("failed to parse {flag} as RFC 3339"))
        .map(|value| value.with_timezone(&Utc))
        .map_err(TickError::Other)
}

fn emit_output<T>(json: bool, value: &T) -> Result<()>
where
    T: Serialize + HumanOutput,
{
    if json {
        let text = serde_json::to_string_pretty(value)
            .context("failed to serialize JSON output")
            .map_err(TickError::Other)?;
        println!("{text}");
    } else {
        println!("{}", value.render_human());
    }
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .try_init();
}

trait HumanOutput {
    fn render_human(&self) -> String;
}

#[derive(Debug, Serialize)]
struct RemoteListOutput {
    command: &'static str,
    filters: RemoteListFilters,
    dataset_total: usize,
    datasets: Vec<RemoteDatasetEntry>,
}

impl RemoteListOutput {
    fn from_entries(filters: RemoteListFilters, datasets: Vec<RemoteDatasetEntry>) -> Self {
        Self {
            command: "list",
            filters,
            dataset_total: datasets.len(),
            datasets,
        }
    }
}

impl HumanOutput for RemoteListOutput {
    fn render_human(&self) -> String {
        let mut lines = vec!["list".to_string()];

        if self.filters.exchange.is_some()
            || self.filters.asset.is_some()
            || self.filters.search.is_some()
        {
            lines.push(format!(
                "filters: exchange={:?} asset={:?} search={:?}",
                self.filters.exchange, self.filters.asset, self.filters.search
            ));
        }

        lines.push(format!("datasets: {}", self.dataset_total));

        if !self.datasets.is_empty() {
            lines.push("remote datasets:".into());
            for dataset in &self.datasets {
                lines.push(format!(
                    "  {}:{} {} -> {}",
                    dataset.exchange, dataset.asset, dataset.start, dataset.end
                ));
            }
        }

        lines.join("\n")
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
struct RemoteListFilters {
    exchange: Option<String>,
    asset: Option<String>,
    search: Option<String>,
}

impl RemoteListFilters {
    fn from_args(args: &RemoteListArgs) -> Self {
        Self {
            exchange: args.exchange.clone(),
            asset: args.asset.clone(),
            search: args.search.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
struct LocalListFilters {
    exchange: Option<String>,
    asset: Option<String>,
    date: Option<String>,
}

impl LocalListFilters {
    fn from_args(args: &LocalListArgs) -> Self {
        Self {
            exchange: args.exchange.clone(),
            asset: args.asset.clone(),
            date: args.date.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct LocalListOutput {
    command: &'static str,
    root: String,
    filters: LocalListFilters,
    snapshot_total: usize,
    snapshots: Vec<LocalSnapshotEntry>,
}

impl LocalListOutput {
    fn from_entries(
        root: String,
        filters: LocalListFilters,
        snapshots: Vec<LocalSnapshotEntry>,
    ) -> Self {
        Self {
            command: "list local",
            root,
            filters,
            snapshot_total: snapshots.len(),
            snapshots,
        }
    }
}

impl HumanOutput for LocalListOutput {
    fn render_human(&self) -> String {
        let mut lines = vec!["list local".to_string(), format!("root: {}", self.root)];

        if self.filters.exchange.is_some()
            || self.filters.asset.is_some()
            || self.filters.date.is_some()
        {
            lines.push(format!(
                "filters: exchange={:?} asset={:?} date={:?}",
                self.filters.exchange, self.filters.asset, self.filters.date
            ));
        }

        lines.push(format!("snapshots: {}", self.snapshot_total));

        if !self.snapshots.is_empty() {
            lines.push("local snapshots:".into());
            for snapshot in self.snapshots.iter().take(50) {
                lines.push(format!("  {}", snapshot.key));
            }
            if self.snapshots.len() > 50 {
                lines.push(format!("  ... {} more", self.snapshots.len() - 50));
            }
        }

        lines.join("\n")
    }
}

fn filter_local_list_entries(
    entries: Vec<LocalSnapshotEntry>,
    filters: &LocalListFilters,
) -> Vec<LocalSnapshotEntry> {
    entries
        .into_iter()
        .filter(|entry| matches_exact(entry.exchange.as_deref(), filters.exchange.as_deref()))
        .filter(|entry| matches_exact(entry.asset.as_deref(), filters.asset.as_deref()))
        .filter(|entry| matches_exact(entry.date.as_deref(), filters.date.as_deref()))
        .collect()
}

fn filter_remote_catalog(
    exchanges: Vec<CatalogExchange>,
    filters: &RemoteListFilters,
    limit: usize,
) -> Vec<RemoteDatasetEntry> {
    let search = filters
        .search
        .as_deref()
        .map(|value| value.to_ascii_lowercase());
    let mut datasets = Vec::new();

    for exchange in exchanges {
        let exchange_id = exchange.id;
        for asset in exchange.assets {
            let dataset = format!("{}:{}", exchange_id, asset.id);
            if !matches_exact(Some(exchange_id.as_str()), filters.exchange.as_deref()) {
                continue;
            }
            if !matches_exact(Some(asset.id.as_str()), filters.asset.as_deref()) {
                continue;
            }
            if let Some(search) = search.as_deref() {
                let haystack = dataset.to_ascii_lowercase();
                if !haystack.contains(search) {
                    continue;
                }
            }
            datasets.push(RemoteDatasetEntry {
                exchange: exchange_id.clone(),
                asset: asset.id.clone(),
                start: asset.start,
                end: asset.end,
                source: asset.source.clone(),
                dataset,
            });
        }
    }

    datasets.sort_by(|left, right| left.dataset.cmp(&right.dataset));
    if datasets.len() > limit {
        datasets.truncate(limit);
    }
    datasets
}

fn matches_exact(value: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        Some(expected) => value == Some(expected),
        None => true,
    }
}

#[derive(Debug, Serialize)]
struct SyncOutput {
    command: &'static str,
    exchange: String,
    asset: String,
    requested_range: TimeWindow,
    effective_range: TimeWindow,
    root: String,
    remote_total: usize,
    downloaded_total: usize,
    skipped_total: usize,
    failed_total: usize,
    materialized_days_total: usize,
    materialization_incomplete_days_total: usize,
    downloaded_keys: Vec<String>,
    failed: Vec<crate::syncer::FailedDownload>,
}

#[derive(Debug, Serialize)]
struct ResetOutput {
    command: &'static str,
    root: String,
    snapshot_total: usize,
    daily_artifact_total: usize,
    removed_roots: Vec<String>,
}

impl SyncOutput {
    fn from_parts(
        plan: &SyncPlan,
        execution: SyncExecution,
        materialization: MaterializeExecution,
    ) -> Self {
        Self {
            command: "sync",
            exchange: plan.exchange.clone(),
            asset: plan.asset.clone(),
            requested_range: plan.requested_range.clone(),
            effective_range: plan.effective_range.clone(),
            root: plan.root.display().to_string(),
            remote_total: plan.remote_total(),
            downloaded_total: execution.downloaded_total(),
            skipped_total: plan.present_total(),
            failed_total: execution.failed_total(),
            materialized_days_total: materialization.built_total,
            materialization_incomplete_days_total: materialization.incomplete_total,
            downloaded_keys: execution.downloaded_keys,
            failed: execution.failed,
        }
    }
}

impl HumanOutput for SyncOutput {
    fn render_human(&self) -> String {
        let mut lines = vec![
            format!("sync {} {}", self.exchange, self.asset),
            format!("root: {}", self.root),
            format!(
                "requested: {} -> {}",
                self.requested_range.from, self.requested_range.to
            ),
            format!(
                "effective: {} -> {}",
                self.effective_range.from, self.effective_range.to
            ),
            format!("remote: {}", self.remote_total),
            format!("downloaded: {}", self.downloaded_total),
            format!("skipped: {}", self.skipped_total),
            format!("failed: {}", self.failed_total),
            format!("materialized days: {}", self.materialized_days_total),
            format!(
                "incomplete days: {}",
                self.materialization_incomplete_days_total
            ),
        ];
        if !self.failed.is_empty() {
            lines.push("failed keys:".into());
            for failure in &self.failed {
                lines.push(format!("  {}: {}", failure.key, failure.error));
            }
        }
        lines.join("\n")
    }
}

impl HumanOutput for ResetOutput {
    fn render_human(&self) -> String {
        let mut lines = vec![
            "reset".to_string(),
            format!("root: {}", self.root),
            format!("removed snapshots: {}", self.snapshot_total),
            format!("removed daily artifacts: {}", self.daily_artifact_total),
        ];
        if !self.removed_roots.is_empty() {
            lines.push("removed roots:".into());
            for root in &self.removed_roots {
                lines.push(format!("  {root}"));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;

    use super::{
        LocalListFilters, LocalListOutput, RemoteListFilters, RemoteListOutput, ResetOutput,
        SyncOutput, TimeWindow, filter_local_list_entries, filter_remote_catalog,
        infer_install_dir_from_executable, looks_like_cargo_target_dir, run_reset,
    };
    use crate::api::{CatalogAsset, CatalogExchange};
    use crate::cli::ResetArgs;
    use crate::config::Config;
    use crate::layout::{Layout, LocalSnapshotEntry};
    use crate::syncer::FailedDownload;
    use crate::tui::RemoteDatasetEntry;

    #[test]
    fn remote_list_json_shape_is_stable() {
        let output = RemoteListOutput {
            command: "list",
            filters: RemoteListFilters {
                exchange: Some("aster".into()),
                asset: Some("BTCUSDT".into()),
                search: Some("btc".into()),
            },
            dataset_total: 1,
            datasets: vec![RemoteDatasetEntry {
                exchange: "aster".into(),
                asset: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 1, 0, 9, 59).unwrap(),
                source: Some("manifest".into()),
                dataset: "aster:BTCUSDT".into(),
            }],
        };
        let json = serde_json::to_string(&output).expect("json");
        assert_eq!(
            json,
            "{\"command\":\"list\",\"filters\":{\"exchange\":\"aster\",\"asset\":\"BTCUSDT\",\"search\":\"btc\"},\"dataset_total\":1,\"datasets\":[{\"exchange\":\"aster\",\"asset\":\"BTCUSDT\",\"start\":\"2026-06-01T00:00:00Z\",\"end\":\"2026-06-01T00:09:59Z\",\"source\":\"manifest\",\"dataset\":\"aster:BTCUSDT\"}]}"
        );
    }

    #[test]
    fn local_list_json_shape_is_stable() {
        let output = LocalListOutput {
            command: "list local",
            root: "/tmp/tick".into(),
            filters: LocalListFilters {
                exchange: Some("aster".into()),
                asset: None,
                date: None,
            },
            snapshot_total: 1,
            snapshots: vec![LocalSnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst".into(),
                path: "/tmp/tick/data/bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst".into(),
                filename: "file.jsonl.zst".into(),
                exchange: Some("aster".into()),
                asset: Some("BTCUSDT".into()),
                date: Some("2026-06-01".into()),
                start: Some(Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap()),
                end: Some(Utc.with_ymd_and_hms(2026, 6, 1, 0, 9, 59).unwrap()),
            }],
        };
        let json = serde_json::to_string(&output).expect("json");
        assert_eq!(
            json,
            "{\"command\":\"list local\",\"root\":\"/tmp/tick\",\"filters\":{\"exchange\":\"aster\",\"asset\":null,\"date\":null},\"snapshot_total\":1,\"snapshots\":[{\"key\":\"bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst\",\"path\":\"/tmp/tick/data/bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst\",\"filename\":\"file.jsonl.zst\",\"exchange\":\"aster\",\"asset\":\"BTCUSDT\",\"date\":\"2026-06-01\",\"start\":\"2026-06-01T00:00:00Z\",\"end\":\"2026-06-01T00:09:59Z\"}]}"
        );
    }

    #[test]
    fn local_list_filters_apply_exact_matches() {
        let entries = vec![
            LocalSnapshotEntry {
                key: "bronze/aster/BTCUSDT/2026-06-01/a.jsonl.zst".into(),
                path: "/tmp/a".into(),
                filename: "a.jsonl.zst".into(),
                exchange: Some("aster".into()),
                asset: Some("BTCUSDT".into()),
                date: Some("2026-06-01".into()),
                start: None,
                end: None,
            },
            LocalSnapshotEntry {
                key: "bronze/binance/ETHUSDT/2026-06-01/b.jsonl.zst".into(),
                path: "/tmp/b".into(),
                filename: "b.jsonl.zst".into(),
                exchange: Some("binance".into()),
                asset: Some("ETHUSDT".into()),
                date: Some("2026-06-01".into()),
                start: None,
                end: None,
            },
        ];

        let filtered = filter_local_list_entries(
            entries,
            &LocalListFilters {
                exchange: Some("aster".into()),
                asset: Some("BTCUSDT".into()),
                date: Some("2026-06-01".into()),
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].exchange.as_deref(), Some("aster"));
        assert_eq!(filtered[0].asset.as_deref(), Some("BTCUSDT"));
    }

    #[test]
    fn infer_install_dir_uses_parent_for_installed_binary() {
        let install_dir =
            infer_install_dir_from_executable(Path::new("/Users/test/.tick/bin/tick"));
        assert_eq!(install_dir, Some(PathBuf::from("/Users/test/.tick/bin")));
    }

    #[test]
    fn infer_install_dir_skips_non_release_binaries() {
        assert_eq!(
            infer_install_dir_from_executable(Path::new("/repo/target/debug/tick")),
            None
        );
        assert_eq!(
            infer_install_dir_from_executable(Path::new("/repo/target/release/tick")),
            None
        );
        assert_eq!(
            infer_install_dir_from_executable(Path::new("/usr/local/bin/not-tick")),
            None
        );
    }

    #[test]
    fn cargo_target_detection_matches_debug_and_release_dirs() {
        assert!(looks_like_cargo_target_dir(Path::new("/repo/target/debug")));
        assert!(looks_like_cargo_target_dir(Path::new(
            "/repo/target/release"
        )));
        assert!(!looks_like_cargo_target_dir(Path::new(
            "/Users/test/.tick/bin"
        )));
    }

    #[test]
    fn remote_list_filters_apply_search_and_limit() {
        let datasets = filter_remote_catalog(
            vec![
                CatalogExchange {
                    id: "aster".into(),
                    assets: vec![
                        CatalogAsset {
                            id: "BTCUSDT".into(),
                            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                            source: Some("manifest".into()),
                        },
                        CatalogAsset {
                            id: "ETHUSDT".into(),
                            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                            source: Some("manifest".into()),
                        },
                    ],
                },
                CatalogExchange {
                    id: "binance".into(),
                    assets: vec![CatalogAsset {
                        id: "BTCUSDT".into(),
                        start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                        source: Some("manifest".into()),
                    }],
                },
            ],
            &RemoteListFilters {
                exchange: None,
                asset: None,
                search: Some("btc".into()),
            },
            1,
        );

        assert_eq!(datasets.len(), 1);
        assert!(datasets[0].dataset.contains("BTCUSDT"));
    }

    #[test]
    fn sync_json_shape_is_stable() {
        let output = SyncOutput {
            command: "sync",
            exchange: "aster".into(),
            asset: "BTCUSDT".into(),
            requested_range: TimeWindow {
                from: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                to: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            },
            effective_range: TimeWindow {
                from: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                to: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            },
            root: "/tmp/tick".into(),
            remote_total: 2,
            downloaded_total: 1,
            skipped_total: 1,
            failed_total: 1,
            materialized_days_total: 1,
            materialization_incomplete_days_total: 1,
            downloaded_keys: vec!["k".into()],
            failed: vec![FailedDownload {
                key: "x".into(),
                error: "boom".into(),
            }],
        };
        let json = serde_json::to_string(&output).expect("json");
        assert_eq!(
            json,
            "{\"command\":\"sync\",\"exchange\":\"aster\",\"asset\":\"BTCUSDT\",\"requested_range\":{\"from\":\"2026-06-01T00:00:00Z\",\"to\":\"2026-06-02T00:00:00Z\"},\"effective_range\":{\"from\":\"2026-06-01T00:00:00Z\",\"to\":\"2026-06-02T00:00:00Z\"},\"root\":\"/tmp/tick\",\"remote_total\":2,\"downloaded_total\":1,\"skipped_total\":1,\"failed_total\":1,\"materialized_days_total\":1,\"materialization_incomplete_days_total\":1,\"downloaded_keys\":[\"k\"],\"failed\":[{\"key\":\"x\",\"error\":\"boom\"}]}"
        );
    }

    #[test]
    fn reset_json_shape_is_stable() {
        let output = ResetOutput {
            command: "reset",
            root: "/tmp/tick".into(),
            snapshot_total: 2,
            daily_artifact_total: 1,
            removed_roots: vec![
                "/tmp/tick/data".into(),
                "/tmp/tick/daily".into(),
                "/tmp/tick/tmp".into(),
                "/tmp/tick/cache".into(),
            ],
        };
        let json = serde_json::to_string(&output).expect("json");
        assert_eq!(
            json,
            "{\"command\":\"reset\",\"root\":\"/tmp/tick\",\"snapshot_total\":2,\"daily_artifact_total\":1,\"removed_roots\":[\"/tmp/tick/data\",\"/tmp/tick/daily\",\"/tmp/tick/tmp\",\"/tmp/tick/cache\"]}"
        );
    }

    #[tokio::test]
    async fn reset_removes_local_dataset_roots() {
        let tempdir = TempDir::new().expect("tempdir");
        let root = tempdir.path().to_path_buf();
        let config = Config {
            base_url: "http://example.test".into(),
            api_key: None,
            api_key_source: None,
            root: root.clone(),
            concurrency: 4,
            timeout: std::time::Duration::from_secs(5),
        };
        let layout = Layout::new(root.clone());

        let snapshot_path = layout
            .data_path_for_key("events/aster/BTCUSDT/aster_BTCUSDT_2026-06-01.jsonl.zst")
            .expect("snapshot path");
        std::fs::create_dir_all(snapshot_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&snapshot_path, b"snapshot").expect("write snapshot");

        let daily_path = layout.daily_path_for_dataset_day(
            "aster",
            "BTCUSDT",
            chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        );
        std::fs::create_dir_all(daily_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&daily_path, b"daily").expect("write daily");

        let tmp_path =
            layout.temp_path_for_key("events/aster/BTCUSDT/aster_BTCUSDT_2026-06-01.jsonl.zst");
        std::fs::create_dir_all(tmp_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&tmp_path, b"partial").expect("write tmp");

        let cache_path = layout.catalog_cache_path("aster", "BTCUSDT");
        std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&cache_path, b"cache").expect("write cache");

        let exit_code = run_reset(&config, ResetArgs { json: false })
            .await
            .expect("reset");
        assert_eq!(exit_code, 0);

        assert!(!layout.data_root().exists());
        assert!(!layout.daily_root().exists());
        assert!(!layout.tmp_root().exists());
        assert!(!layout.cache_root().exists());
        assert!(layout.root().exists());
        assert_eq!(layout.list_local_snapshots().expect("snapshots").len(), 0);
        assert_eq!(
            layout
                .list_local_daily_artifacts()
                .expect("daily artifacts")
                .len(),
            0
        );

        let remaining_roots = [
            layout.data_root(),
            layout.daily_root(),
            layout.tmp_root(),
            layout.cache_root(),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<BTreeSet<_>>();
        assert!(remaining_roots.is_empty());
    }
}
