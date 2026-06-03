use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::net::TcpListener;

use polaris::api::{
    CatalogAsset, CatalogExchange, CatalogResponse, DatasetAccess, DatasetAccessStatus,
    PolarisClient, SnapshotEntry,
};
use polaris::config::Config;
use polaris::error::TickError;
use polaris::layout::Layout;
use polaris::planner::{LocalSnapshotState, TimeWindow, build_sync_plan};
use polaris::syncer::{acquire_sync_lock, execute_sync};

#[derive(Clone)]
struct TestServerState {
    base_url: String,
    exchange: String,
    asset: String,
    coverage: TimeWindow,
    pages: Vec<Vec<SnapshotEntry>>,
    total_bytes: u64,
    files: HashMap<String, Vec<u8>>,
    failures_remaining: Arc<Mutex<HashMap<String, usize>>>,
    asset_available: bool,
}

#[derive(Debug, Deserialize)]
struct SnapshotsQuery {
    exchange: String,
    asset: String,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotDownloadQuery {
    key: String,
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogQuery {
    exchange: Option<String>,
    asset: Option<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotsResponse {
    total: usize,
    total_bytes: u64,
    next_cursor: Option<String>,
    snapshots: Vec<SnapshotEntry>,
}

#[tokio::test]
async fn catalog_and_snapshot_pagination_drive_the_plan() {
    let fixture = SnapshotFixture::basic();
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");

    let plan = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect("plan");

    assert_eq!(plan.remote_total(), 2);
    assert_eq!(plan.total_remote_bytes, fixture.total_bytes);
    assert_eq!(plan.missing_total(), 2);
}

#[tokio::test]
async fn remote_catalog_can_be_listed_without_filters() {
    let fixture = SnapshotFixture::basic();
    let server = TestServer::spawn(fixture.clone()).await;
    let client =
        PolarisClient::new(server.base_url(), None, Duration::from_secs(5)).expect("client");

    let catalog = client.fetch_catalog(None, None).await.expect("catalog");

    assert_eq!(catalog.exchanges.len(), 1);
    assert_eq!(catalog.exchanges[0].id, fixture.exchange);
    assert_eq!(catalog.exchanges[0].assets.len(), 1);
    assert_eq!(catalog.exchanges[0].assets[0].id, fixture.asset);
}

#[tokio::test]
async fn missing_catalog_asset_returns_dataset_unavailable() {
    let mut fixture = SnapshotFixture::basic();
    fixture.asset_available = false;
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");

    let err = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect_err("dataset unavailable");

    assert!(matches!(err, TickError::DatasetUnavailable(_)));
}

#[tokio::test]
async fn sync_downloads_files_from_standardized_snapshot_urls() {
    let fixture = SnapshotFixture::basic();
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");

    let plan = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect("plan");
    let execution = execute_sync(&client, &plan, 2).await;

    assert_eq!(execution.downloaded_total(), 2);
    assert_eq!(execution.failed_total(), 0);

    let layout = Layout::new(config.root.clone());
    for (key, expected) in &fixture.files {
        let path = layout.data_path_for_key(key).expect("path");
        let bytes = tokio::fs::read(path).await.expect("downloaded file");
        assert_eq!(&bytes, expected);
    }
}

#[tokio::test]
async fn existing_files_are_skipped_during_sync() {
    let fixture = SnapshotFixture::basic();
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");
    let layout = Layout::new(config.root.clone());

    let existing_key = fixture.pages[0][0].key.clone();
    let existing_path = layout.data_path_for_key(&existing_key).expect("path");
    tokio::fs::create_dir_all(existing_path.parent().expect("parent"))
        .await
        .expect("mkdir");
    tokio::fs::write(&existing_path, b"already-here")
        .await
        .expect("write");

    let plan = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect("plan");
    let execution = execute_sync(&client, &plan, 2).await;

    assert_eq!(plan.present_total(), 1);
    assert_eq!(plan.missing_total(), 1);
    assert_eq!(execution.downloaded_total(), 1);
    assert_eq!(execution.failed_total(), 0);
}

#[tokio::test]
async fn failed_download_is_retried_and_then_succeeds() {
    let mut fixture = SnapshotFixture::single();
    let key = fixture.pages[0][0].key.clone();
    fixture.failures_remaining.insert(key.clone(), 1);

    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");

    let plan = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect("plan");
    let execution = execute_sync(&client, &plan, 1).await;

    assert_eq!(execution.downloaded_total(), 1);
    assert_eq!(execution.failed_total(), 0);
}

#[test]
fn second_sync_lock_is_rejected() {
    let tempdir = TempDir::new().expect("tempdir");
    let layout = Layout::new(tempdir.path().to_path_buf());
    let _first = acquire_sync_lock(&layout).expect("first lock");
    let err = acquire_sync_lock(&layout).expect_err("lock should fail");
    assert!(matches!(err, TickError::LockHeld(_)));
}

#[tokio::test]
async fn existing_part_file_is_replaced_cleanly() {
    let fixture = SnapshotFixture::single();
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");
    let layout = Layout::new(config.root.clone());
    let key = fixture.pages[0][0].key.clone();
    let temp_path = layout.temp_path_for_key(&key);

    tokio::fs::create_dir_all(temp_path.parent().expect("tmp parent"))
        .await
        .expect("mkdir");
    tokio::fs::write(&temp_path, b"partial")
        .await
        .expect("write partial");

    let plan = build_sync_plan(
        &client,
        &config,
        &fixture.exchange,
        &fixture.asset,
        fixture.requested_range(),
    )
    .await
    .expect("plan");

    assert_eq!(plan.snapshots[0].state, LocalSnapshotState::Incomplete);

    let execution = execute_sync(&client, &plan, 1).await;
    assert_eq!(execution.failed_total(), 0);
    assert!(!temp_path.exists());

    let final_path = layout.data_path_for_key(&key).expect("final path");
    let bytes = tokio::fs::read(final_path).await.expect("final bytes");
    assert_eq!(bytes, fixture.files[&key]);
}

#[derive(Clone)]
struct SnapshotFixture {
    exchange: String,
    asset: String,
    coverage: TimeWindow,
    pages: Vec<Vec<SnapshotEntry>>,
    files: HashMap<String, Vec<u8>>,
    total_bytes: u64,
    failures_remaining: HashMap<String, usize>,
    asset_available: bool,
}

impl SnapshotFixture {
    fn basic() -> Self {
        let exchange = "aster".to_string();
        let asset = "BTCUSDT".to_string();
        let coverage = TimeWindow {
            from: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        };
        let key_a = "bronze/aster/BTCUSDT/2026-06-01/a.jsonl.zst".to_string();
        let key_b = "bronze/aster/BTCUSDT/2026-06-01/b.jsonl.zst".to_string();
        let pages = vec![
            vec![SnapshotEntry {
                key: key_a.clone(),
                filename: "a.jsonl.zst".into(),
            }],
            vec![SnapshotEntry {
                key: key_b.clone(),
                filename: "b.jsonl.zst".into(),
            }],
        ];
        let files = HashMap::from([
            (key_a, b"first-file".to_vec()),
            (key_b, b"second-file".to_vec()),
        ]);
        let total_bytes = files.values().map(|value| value.len() as u64).sum();
        Self {
            exchange,
            asset,
            coverage,
            pages,
            files,
            total_bytes,
            failures_remaining: HashMap::new(),
            asset_available: true,
        }
    }

    fn single() -> Self {
        let mut fixture = Self::basic();
        fixture.pages.truncate(1);
        fixture.files.retain(|key, _| key.ends_with("a.jsonl.zst"));
        fixture.total_bytes = fixture.files.values().map(|value| value.len() as u64).sum();
        fixture
    }

    fn requested_range(&self) -> TimeWindow {
        self.coverage.clone()
    }
}

struct TestServer {
    addr: SocketAddr,
}

impl TestServer {
    async fn spawn(fixture: SnapshotFixture) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let base_url = format!("http://{addr}");
        let state = TestServerState {
            base_url: base_url.clone(),
            exchange: fixture.exchange,
            asset: fixture.asset,
            coverage: fixture.coverage,
            pages: fixture.pages,
            total_bytes: fixture.total_bytes,
            files: fixture.files,
            failures_remaining: Arc::new(Mutex::new(fixture.failures_remaining)),
            asset_available: fixture.asset_available,
        };
        let app = Router::new()
            .route("/catalog", get(handle_catalog))
            .route("/snapshots", get(handle_snapshots))
            .route("/snapshots/download", get(handle_snapshot_download))
            .route("/files/{*key}", get(handle_file))
            .with_state(state);

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });

        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

fn config_for_test(base_url: String, root: &std::path::Path) -> Config {
    Config {
        base_url,
        api_key: None,
        api_key_source: None,
        root: root.to_path_buf(),
        concurrency: 4,
        timeout: Duration::from_secs(5),
    }
}

async fn handle_catalog(
    State(state): State<TestServerState>,
    Query(query): Query<CatalogQuery>,
) -> Json<CatalogResponse> {
    let include_exchange = query
        .exchange
        .as_deref()
        .is_none_or(|value| value == state.exchange);
    let include_asset = query
        .asset
        .as_deref()
        .is_none_or(|value| value == state.asset);
    let assets = if state.asset_available && include_exchange && include_asset {
        vec![CatalogAsset {
            id: state.asset.clone(),
            start: state.coverage.from,
            end: state.coverage.to,
            source: Some("manifest".into()),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: Some(
                    chrono::NaiveDate::from_ymd_opt(2026, 5, 28).unwrap(),
                ),
            }),
        }]
    } else {
        Vec::new()
    };

    Json(CatalogResponse {
        exchanges: vec![CatalogExchange {
            id: query.exchange.unwrap_or(state.exchange.clone()),
            assets,
        }],
        updated_at: Some(state.coverage.to.to_rfc3339()),
    })
}

async fn handle_snapshots(
    State(state): State<TestServerState>,
    Query(query): Query<SnapshotsQuery>,
) -> Json<SnapshotsResponse> {
    assert_eq!(query.exchange, state.exchange);
    assert_eq!(query.asset, state.asset);
    let page_index = match query.cursor.as_deref() {
        None => 0,
        Some("page2") => 1,
        _ => 999,
    };
    let snapshots = state.pages.get(page_index).cloned().unwrap_or_default();
    let next_cursor = if page_index + 1 < state.pages.len() {
        Some("page2".to_string())
    } else {
        None
    };
    Json(SnapshotsResponse {
        total: state.pages.iter().map(Vec::len).sum(),
        total_bytes: state.total_bytes,
        next_cursor,
        snapshots,
    })
}

async fn handle_snapshot_download(
    State(state): State<TestServerState>,
    Query(query): Query<SnapshotDownloadQuery>,
) -> Response {
    let _ = query.filename.as_deref();
    if !state.files.contains_key(&query.key) {
        return (StatusCode::NOT_FOUND, "missing").into_response();
    }

    axum::response::Redirect::temporary(&format!("{}/files/{}", state.base_url, query.key))
        .into_response()
}

async fn handle_file(State(state): State<TestServerState>, Path(key): Path<String>) -> Response {
    let mut failures = state.failures_remaining.lock().expect("failures");
    if let Some(remaining) = failures.get_mut(&key) {
        if *remaining > 0 {
            *remaining -= 1;
            return (StatusCode::INTERNAL_SERVER_ERROR, "temporary failure").into_response();
        }
    }
    drop(failures);

    match state.files.get(&key) {
        Some(bytes) => (StatusCode::OK, bytes.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}
