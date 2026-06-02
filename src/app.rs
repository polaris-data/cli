use std::process::ExitCode;

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::api::{CatalogExchange, PolarisClient};
use crate::cli::{
    Cli, Command, DatasetArgs, ListCommand, ListSubcommand, LocalListArgs, RemoteListArgs, SyncArgs,
};
use crate::config::Config;
use crate::error::{Result, TickError};
use crate::layout::{Layout, LocalSnapshotEntry};
use crate::materialize::{MaterializeExecution, materialize_range_days};
use crate::planner::{SyncPlan, TimeWindow, build_sync_plan};
use crate::syncer::{SyncExecution, acquire_sync_lock, execute_sync, layout_for_root};
use crate::tui::{RemoteDatasetEntry, RemoteTuiSeed, can_render_tui, run_remote_list_tui};

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
    let config = Config::from_env()?;
    match cli.command {
        None => run_browser(&config).await,
        Some(Command::List(args)) => run_list(&config, args).await,
        Some(Command::Sync(args)) => {
            let client = PolarisClient::new(
                config.base_url.clone(),
                config.api_key.clone(),
                config.timeout,
            )?;
            run_sync(&config, &client, args).await
        }
    }
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        LocalListFilters, LocalListOutput, RemoteListFilters, RemoteListOutput, SyncOutput,
        TimeWindow, filter_local_list_entries, filter_remote_catalog,
    };
    use crate::api::{CatalogAsset, CatalogExchange};
    use crate::layout::LocalSnapshotEntry;
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
}
