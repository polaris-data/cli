use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;

use polaris::api::{
    AccountResponse, CatalogResponse, CatalogMarket, CatalogSource, CliAuthPollResponse,
    CliAuthStartResponse, DatasetAccess, DatasetAccessStatus, FeedbackResponse, PolarisClient,
    SnapshotEntry,
};
use polaris::config::Config;
use polaris::error::TickError;
use polaris::layout::Layout;
use polaris::planner::{LocalSnapshotState, TimeWindow, build_sync_plan};
use polaris::syncer::{acquire_sync_lock, execute_sync};

#[derive(Clone)]
struct TestServerState {
    base_url: String,
    source: String,
    market: String,
    coverage: TimeWindow,
    pages: Vec<Vec<SnapshotEntry>>,
    total_bytes: u64,
    files: HashMap<String, Vec<u8>>,
    failures_remaining: Arc<Mutex<HashMap<String, usize>>>,
    market_available: bool,
}

#[derive(Debug, Deserialize)]
struct SnapshotsQuery {
    source: String,
    market: String,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotDownloadQuery {
    key: String,
}

#[derive(Debug, Deserialize)]
struct CatalogQuery {
    source: Option<String>,
    market: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeedbackRequest {
    message: String,
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
        &fixture.source,
        &fixture.market,
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

    assert_eq!(catalog.sources.len(), 1);
    assert_eq!(catalog.sources[0].id, fixture.source);
    assert_eq!(catalog.sources[0].markets.len(), 1);
    assert_eq!(catalog.sources[0].markets[0].id, fixture.market);
}

#[tokio::test]
async fn missing_catalog_market_returns_dataset_unavailable() {
    let mut fixture = SnapshotFixture::basic();
    fixture.market_available = false;
    let server = TestServer::spawn(fixture.clone()).await;
    let tempdir = TempDir::new().expect("tempdir");
    let config = config_for_test(server.base_url(), tempdir.path());
    let client =
        PolarisClient::new(config.base_url.clone(), None, Duration::from_secs(5)).expect("client");

    let err = build_sync_plan(
        &client,
        &config,
        &fixture.source,
        &fixture.market,
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
        &fixture.source,
        &fixture.market,
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
        &fixture.source,
        &fixture.market,
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
        &fixture.source,
        &fixture.market,
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
        &fixture.source,
        &fixture.market,
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

#[tokio::test]
async fn cli_auth_start_returns_expected_browser_login_payload() {
    let server = CliAuthTestServer::spawn().await;
    let client =
        PolarisClient::new(server.base_url(), None, Duration::from_secs(5)).expect("client");

    let response = client.start_cli_auth().await.expect("start auth");

    assert_eq!(
        response,
        CliAuthStartResponse {
            request_id: "req-123".into(),
            poll_token: "poll-456".into(),
            user_code: "ABCD-EFGH".into(),
            login_url: "https://polaris.supply/login?request_id=req-123&code=ABCD-EFGH".into(),
            expires_at: Utc.with_ymd_and_hms(2026, 6, 7, 12, 0, 0).unwrap(),
            interval_ms: 1000,
        }
    );
}

#[tokio::test]
async fn cli_auth_poll_parses_pending_approved_and_consumed_responses() {
    let server = CliAuthTestServer::spawn().await;
    let client =
        PolarisClient::new(server.base_url(), None, Duration::from_secs(5)).expect("client");

    let pending = client
        .poll_cli_auth("req-123", "poll-456")
        .await
        .expect("pending poll");
    assert_eq!(
        pending,
        CliAuthPollResponse::Pending {
            request_id: "req-123".into(),
            expires_at: Utc.with_ymd_and_hms(2026, 6, 7, 12, 0, 0).unwrap(),
            interval_ms: 1000,
        }
    );

    let approved = client
        .poll_cli_auth("req-123", "poll-456")
        .await
        .expect("approved poll");
    assert_eq!(
        approved,
        CliAuthPollResponse::Approved {
            request_id: "req-123".into(),
            user_id: "user-789".into(),
            display_name: Some("CLI User".into()),
            email: Some("user@example.com".into()),
            wallet_address: Some("0xabc".into()),
            avatar_url: Some("https://example.com/avatar.png".into()),
            api_key: "polaris_key_cli_generated".into(),
        }
    );

    let consumed = client
        .poll_cli_auth("req-123", "poll-456")
        .await
        .expect("consumed poll");
    assert_eq!(consumed, CliAuthPollResponse::Consumed);
}

#[tokio::test]
async fn account_endpoint_returns_live_identity_for_api_key_sessions() {
    let server = AccountTestServer::spawn().await;
    let client = PolarisClient::new(
        server.base_url(),
        Some("polaris_key_example".into()),
        Duration::from_secs(5),
    )
    .expect("client");

    let account = client.fetch_account().await.expect("account");

    assert_eq!(
        account,
        AccountResponse {
            user_id: "user-live".into(),
            auth: polaris::api::AccountAuth {
                provider: "api_key".into(),
                key_id: Some("key-live".into()),
            },
            identity: polaris::api::AccountIdentity {
                display_name: Some("Live User".into()),
                email: Some("live@example.com".into()),
                wallet_address: Some("0xlive".into()),
                avatar_url: Some("https://example.com/live.png".into()),
                created_at: Some(Utc.with_ymd_and_hms(2026, 6, 7, 12, 0, 0).unwrap()),
                updated_at: Some(Utc.with_ymd_and_hms(2026, 6, 7, 12, 5, 0).unwrap()),
            },
            subscription: polaris::api::AccountSubscription {
                tier: "free".into(),
            },
        }
    );
}

#[tokio::test]
async fn feedback_endpoint_posts_message_and_bearer_token() {
    let server = FeedbackTestServer::spawn().await;
    let client = PolarisClient::new(
        server.base_url(),
        Some("polaris_key_example".into()),
        Duration::from_secs(5),
    )
    .expect("client");

    let response = client
        .submit_feedback("can you add parquet downloads?")
        .await
        .expect("feedback");

    assert_eq!(response, FeedbackResponse { ok: true });
    assert_eq!(
        server.take_requests(),
        vec![CapturedFeedbackRequest {
            authorization: Some("Bearer polaris_key_example".into()),
            message: "can you add parquet downloads?".into(),
        }]
    );
}

#[derive(Clone)]
struct CliAuthTestServerState {
    poll_calls: Arc<Mutex<usize>>,
}

#[derive(Debug, Deserialize)]
struct CliAuthPollQuery {
    request_id: String,
    poll_token: String,
}

struct CliAuthTestServer {
    addr: SocketAddr,
}

impl CliAuthTestServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = Router::new()
            .route("/auth/cli/start", post(handle_cli_auth_start))
            .route("/auth/cli/poll", get(handle_cli_auth_poll))
            .with_state(CliAuthTestServerState {
                poll_calls: Arc::new(Mutex::new(0)),
            });

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });

        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn handle_cli_auth_start() -> Response {
    (
        StatusCode::CREATED,
        Json(json!({
            "request_id": "req-123",
            "poll_token": "poll-456",
            "user_code": "ABCD-EFGH",
            "login_url": "https://polaris.supply/login?request_id=req-123&code=ABCD-EFGH",
            "expires_at": "2026-06-07T12:00:00Z",
            "interval_ms": 1000
        })),
    )
        .into_response()
}

async fn handle_cli_auth_poll(
    State(state): State<CliAuthTestServerState>,
    Query(query): Query<CliAuthPollQuery>,
) -> Response {
    assert_eq!(query.request_id, "req-123");
    assert_eq!(query.poll_token, "poll-456");

    let mut poll_calls = state.poll_calls.lock().expect("poll calls");
    let response = match *poll_calls {
        0 => (
            StatusCode::OK,
            Json(json!({
                "status": "pending",
                "request_id": "req-123",
                "expires_at": "2026-06-07T12:00:00Z",
                "interval_ms": 1000
            })),
        )
            .into_response(),
        1 => (
            StatusCode::OK,
            Json(json!({
                "status": "approved",
                "request_id": "req-123",
                "user_id": "user-789",
                "display_name": "CLI User",
                "email": "user@example.com",
                "wallet_address": "0xabc",
                "avatar_url": "https://example.com/avatar.png",
                "api_key": "polaris_key_cli_generated"
            })),
        )
            .into_response(),
        _ => (StatusCode::GONE, Json(json!({ "status": "consumed" }))).into_response(),
    };
    *poll_calls += 1;

    response
}

struct AccountTestServer {
    addr: SocketAddr,
}

impl AccountTestServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = Router::new().route("/account", get(handle_account));

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });

        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn handle_account() -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "user_id": "user-live",
            "auth": {
                "provider": "api_key",
                "key_id": "key-live"
            },
            "identity": {
                "display_name": "Live User",
                "email": "live@example.com",
                "wallet_address": "0xlive",
                "avatar_url": "https://example.com/live.png",
                "created_at": "2026-06-07T12:00:00Z",
                "updated_at": "2026-06-07T12:05:00Z"
            },
            "subscription": {
                "tier": "free",
                "events_limit": 1000,
                "usage": { "events": 12 },
                "started_at": "2026-06-01T00:00:00Z",
                "expires_at": null,
                "reset_at": "2026-07-01T00:00:00Z"
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedFeedbackRequest {
    authorization: Option<String>,
    message: String,
}

#[derive(Clone)]
struct FeedbackTestServerState {
    requests: Arc<Mutex<Vec<CapturedFeedbackRequest>>>,
}

struct FeedbackTestServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<CapturedFeedbackRequest>>>,
}

impl FeedbackTestServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/feedback", post(handle_feedback))
            .with_state(FeedbackTestServerState {
                requests: requests.clone(),
            });

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });

        Self { addr, requests }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn take_requests(&self) -> Vec<CapturedFeedbackRequest> {
        std::mem::take(&mut *self.requests.lock().expect("requests"))
    }
}

async fn handle_feedback(
    State(state): State<FeedbackTestServerState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<FeedbackRequest>,
) -> Response {
    state
        .requests
        .lock()
        .expect("requests")
        .push(CapturedFeedbackRequest {
            authorization: headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            message: payload.message,
        });

    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

#[derive(Clone)]
struct SnapshotFixture {
    source: String,
    market: String,
    coverage: TimeWindow,
    pages: Vec<Vec<SnapshotEntry>>,
    files: HashMap<String, Vec<u8>>,
    total_bytes: u64,
    failures_remaining: HashMap<String, usize>,
    market_available: bool,
}

impl SnapshotFixture {
    fn basic() -> Self {
        let source = "aster".to_string();
        let market = "BTCUSDT".to_string();
        let coverage = TimeWindow {
            from: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        };
        let key_a = "standard-aster-BTCUSDT-2026-06-01-a".to_string();
        let key_b = "standard-aster-BTCUSDT-2026-06-01-b".to_string();
        let pages = vec![
            vec![SnapshotEntry {
                key: key_a.clone(),
                date: Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
            }],
            vec![SnapshotEntry {
                key: key_b.clone(),
                date: Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
            }],
        ];
        let files = HashMap::from([
            (key_a, b"first-file".to_vec()),
            (key_b, b"second-file".to_vec()),
        ]);
        let total_bytes = files.values().map(|value| value.len() as u64).sum();
        Self {
            source,
            market,
            coverage,
            pages,
            files,
            total_bytes,
            failures_remaining: HashMap::new(),
            market_available: true,
        }
    }

    fn single() -> Self {
        let mut fixture = Self::basic();
        fixture.pages.truncate(1);
        fixture.files.retain(|key, _| key.ends_with("-a"));
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
            source: fixture.source,
            market: fixture.market,
            coverage: fixture.coverage,
            pages: fixture.pages,
            total_bytes: fixture.total_bytes,
            files: fixture.files,
            failures_remaining: Arc::new(Mutex::new(fixture.failures_remaining)),
            market_available: fixture.market_available,
        };
        let app = Router::new()
            .route("/catalog", get(handle_catalog))
            .route("/snapshots", get(handle_snapshots))
            .route("/download", get(handle_snapshot_download))
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
    let include_source = query
        .source
        .as_deref()
        .is_none_or(|value| value == state.source);
    let include_market = query
        .market
        .as_deref()
        .is_none_or(|value| value == state.market);
    let markets = if state.market_available && include_source && include_market {
        vec![CatalogMarket {
            id: state.market.clone(),
            start: state.coverage.from,
            end: state.coverage.to,
            catalog_source: Some("manifest".into()),
            categories: Vec::new(),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
            }),
        }]
    } else {
        Vec::new()
    };

    Json(CatalogResponse {
        sources: vec![CatalogSource {
            id: query.source.unwrap_or(state.source.clone()),
            markets,
        }],
        updated_at: Some(state.coverage.to.to_rfc3339()),
    })
}

async fn handle_snapshots(
    State(state): State<TestServerState>,
    Query(query): Query<SnapshotsQuery>,
) -> Json<SnapshotsResponse> {
    assert_eq!(query.source, state.source);
    assert_eq!(query.market, state.market);
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
    if !state.files.contains_key(&query.key) {
        return (StatusCode::NOT_FOUND, "missing").into_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "url": format!("{}/files/{}", state.base_url, query.key),
            "filename": format!("{}.jsonl.zst", query.key),
            "expires_in_seconds": 3600,
        })),
    )
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
