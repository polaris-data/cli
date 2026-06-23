use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::NaiveDate;
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::mpsc::unbounded_channel;

use super::{
    AccountIdentity, ActiveDaySync, ApiKeyRequirement, BrowserCategory, DatasetView, DaySyncUpdate,
    FileManagerTarget, RemoteDatasetEntry, RemoteListTui, RemoteTuiSeed,
    api_key_requirement_for_download, build_day_coverages, diff_missing_snapshot_keys,
    format_snapshot_location, load_account_identity, load_bookmarks, save_account_identity,
    save_bookmarks, snapshot_reveal_target,
};
use crate::api::{DatasetAccess, DatasetAccessStatus, PolarisClient, SnapshotEntry};
use crate::config::{ApiKeySource, Config};
use crate::layout::LocalSnapshotEntry;

#[derive(Clone)]
struct SnapshotListTestServerState {
    source: String,
    market: String,
    snapshots: Vec<SnapshotEntry>,
    total_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct SnapshotListQuery {
    source: String,
    market: String,
}

#[derive(Debug, Serialize)]
struct SnapshotListResponse {
    total: usize,
    total_bytes: u64,
    next_cursor: Option<String>,
    snapshots: Vec<SnapshotEntry>,
}

struct SnapshotListTestServer {
    addr: SocketAddr,
}

impl SnapshotListTestServer {
    async fn spawn(
        source: String,
        market: String,
        snapshots: Vec<SnapshotEntry>,
        total_bytes: u64,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = Router::new()
            .route("/snapshots", get(handle_test_snapshots))
            .with_state(SnapshotListTestServerState {
                source,
                market,
                snapshots,
                total_bytes,
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

struct AccountTestServer {
    addr: SocketAddr,
}

impl AccountTestServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = Router::new().route("/account", get(handle_test_account));

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });

        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn handle_test_snapshots(
    State(state): State<SnapshotListTestServerState>,
    Query(query): Query<SnapshotListQuery>,
) -> Json<SnapshotListResponse> {
    assert_eq!(query.source, state.source);
    assert_eq!(query.market, state.market);

    Json(SnapshotListResponse {
        total: state.snapshots.len(),
        total_bytes: state.total_bytes,
        next_cursor: None,
        snapshots: state.snapshots,
    })
}

async fn handle_test_account() -> Json<serde_json::Value> {
    Json(serde_json::json!({
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
    }))
}

#[test]
fn search_filters_remote_datasets() {
    let datasets = vec![
        RemoteDatasetEntry {
            source: "aster".into(),
            market: "BTCUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            catalog_source: Some("manifest".into()),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Open,
                public_cutoff_date: None,
            }),
            categories: Vec::new(),
            dataset: "aster:BTCUSDT".into(),
        },
        RemoteDatasetEntry {
            source: "binance".into(),
            market: "ETHUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            catalog_source: Some("manifest".into()),
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
            source: "aster".into(),
            market: "BTCUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            catalog_source: Some("manifest".into()),
            access: Some(DatasetAccess {
                status: DatasetAccessStatus::Preview,
                public_cutoff_date: Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap()),
            }),
            categories: Vec::new(),
            dataset: "aster:BTCUSDT".into(),
        },
        RemoteDatasetEntry {
            source: "binance".into(),
            market: "ETHUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            catalog_source: Some("manifest".into()),
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
    save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()])).expect("save bookmarks");

    let app = RemoteListTui::new(
        vec![
            RemoteDatasetEntry {
                source: "aster".into(),
                market: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Open,
                    public_cutoff_date: None,
                }),
                categories: Vec::new(),
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                source: "binance".into(),
                market: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
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
                source: "aster".into(),
                market: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Open,
                    public_cutoff_date: None,
                }),
                categories: Vec::new(),
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                source: "binance".into(),
                market: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
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
    save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()])).expect("save bookmarks");

    let mut app = RemoteListTui::new(
        vec![
            RemoteDatasetEntry {
                source: "aster".into(),
                market: "BTCUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
                access: Some(DatasetAccess {
                    status: DatasetAccessStatus::Open,
                    public_cutoff_date: None,
                }),
                categories: vec!["Spot".into()],
                dataset: "aster:BTCUSDT".into(),
            },
            RemoteDatasetEntry {
                source: "binance".into(),
                market: "ETHUSDT".into(),
                start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
                catalog_source: Some("manifest".into()),
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
    save_bookmarks(tempdir.path(), &BTreeSet::from([bookmarked.clone()])).expect("save bookmarks");

    let mut app = RemoteListTui::new(
        vec![RemoteDatasetEntry {
            source: "binance".into(),
            market: "ETHUSDT".into(),
            start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            catalog_source: Some("manifest".into()),
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
fn applying_runtime_config_updates_mock_account_view() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let mut app = RemoteListTui::new(
        Vec::new(),
        Vec::<LocalSnapshotEntry>::new(),
        tempdir.path().join("seed-root"),
        4,
        RemoteTuiSeed::default(),
    );
    let config = Config {
        base_url: "https://staging-api.polaris.supply".into(),
        api_key: Some("pk_live_example".into()),
        api_key_source: Some(ApiKeySource::CredentialStore),
        root: tempdir.path().join("runtime-root"),
        concurrency: 8,
        timeout: Duration::from_secs(90),
    };

    app.apply_runtime_config(&config);

    assert!(app.account_view.api_key_present);
    assert_eq!(app.account_view.base_url, config.base_url);
    assert_eq!(app.account_view.root, config.root);
    assert_eq!(app.account_view.api_key_source_label, "stored credential");
}

#[test]
fn applying_runtime_config_loads_persisted_account_identity_when_api_key_exists() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime-root");
    let identity = AccountIdentity {
        user_id: "user-123".into(),
        display_name: Some("CLI User".into()),
        email: Some("user@example.com".into()),
        plan: Some("free".into()),
        wallet_address: Some("0xabc".into()),
        avatar_url: Some("https://example.com/avatar.png".into()),
    };
    save_account_identity(&runtime_root, &identity).expect("save identity");

    let mut app = RemoteListTui::new(
        Vec::new(),
        Vec::<LocalSnapshotEntry>::new(),
        tempdir.path().join("seed-root"),
        4,
        RemoteTuiSeed::default(),
    );
    let config = Config {
        base_url: "https://staging-api.polaris.supply".into(),
        api_key: Some("polaris_key_example".into()),
        api_key_source: Some(ApiKeySource::CredentialStore),
        root: runtime_root,
        concurrency: 8,
        timeout: Duration::from_secs(90),
    };

    app.apply_runtime_config(&config);

    assert_eq!(app.account_view.identity, Some(identity));
}

#[test]
fn applying_runtime_config_ignores_persisted_identity_without_api_key() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime-root");
    let identity = AccountIdentity {
        user_id: "user-123".into(),
        display_name: Some("CLI User".into()),
        email: Some("user@example.com".into()),
        plan: Some("free".into()),
        wallet_address: Some("0xabc".into()),
        avatar_url: Some("https://example.com/avatar.png".into()),
    };
    save_account_identity(&runtime_root, &identity).expect("save identity");

    let mut app = RemoteListTui::new(
        Vec::new(),
        Vec::<LocalSnapshotEntry>::new(),
        tempdir.path().join("seed-root"),
        4,
        RemoteTuiSeed::default(),
    );
    let config = Config {
        base_url: "https://staging-api.polaris.supply".into(),
        api_key: None,
        api_key_source: None,
        root: runtime_root,
        concurrency: 8,
        timeout: Duration::from_secs(90),
    };

    app.apply_runtime_config(&config);

    assert_eq!(app.account_view.identity, None);
}

#[tokio::test]
async fn hydrating_account_identity_uses_live_account_endpoint_and_updates_cache() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime-root");
    let mut app = RemoteListTui::new(
        Vec::new(),
        Vec::<LocalSnapshotEntry>::new(),
        runtime_root.clone(),
        4,
        RemoteTuiSeed::default(),
    );
    let config = Config {
        base_url: "https://staging-api.polaris.supply".into(),
        api_key: Some("polaris_key_example".into()),
        api_key_source: Some(ApiKeySource::CredentialStore),
        root: runtime_root.clone(),
        concurrency: 4,
        timeout: Duration::from_secs(60),
    };
    app.apply_runtime_config(&config);

    let server = AccountTestServer::spawn().await;
    let client = PolarisClient::new(
        server.base_url(),
        config.api_key.clone(),
        Duration::from_secs(1),
    )
    .expect("client");

    app.hydrate_account_identity(&client)
        .await
        .expect("hydrate identity");

    let expected = AccountIdentity {
        user_id: "user-live".into(),
        display_name: Some("Live User".into()),
        email: Some("live@example.com".into()),
        plan: Some("free".into()),
        wallet_address: Some("0xlive".into()),
        avatar_url: Some("https://example.com/live.png".into()),
    };
    assert_eq!(app.account_view.identity, Some(expected.clone()));
    assert_eq!(
        load_account_identity(&runtime_root).expect("load identity"),
        Some(expected)
    );
}

#[test]
fn account_view_is_reachable_without_discarding_browser_selection() {
    let dataset = RemoteDatasetEntry {
        source: "aster".into(),
        market: "BTCUSDT".into(),
        start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        catalog_source: Some("manifest".into()),
        access: Some(DatasetAccess {
            status: DatasetAccessStatus::Open,
            public_cutoff_date: None,
        }),
        categories: Vec::new(),
        dataset: "aster:BTCUSDT".into(),
    };
    let mut app = RemoteListTui::new(
        vec![dataset],
        Vec::<LocalSnapshotEntry>::new(),
        PathBuf::from("/tmp/polaris"),
        4,
        RemoteTuiSeed::default(),
    );

    app.open_account_view();

    assert!(matches!(app.mode, super::ViewMode::Account));
    assert_eq!(
        app.selected_dataset().map(|entry| entry.dataset.as_str()),
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
        },
        SnapshotEntry {
            key: "bronze/aster/BTCUSDT/2026-06-01/b.jsonl.zst".into(),
        },
        SnapshotEntry {
            key: "bronze/aster/BTCUSDT/2026-06-02/c.jsonl.zst".into(),
        },
        SnapshotEntry {
            key: "bronze/aster/BTCUSDT/2026-06-03/d.jsonl.zst".into(),
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
        source: "aster".into(),
        market: "BTCUSDT".into(),
        start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        catalog_source: Some("manifest".into()),
        access: Some(DatasetAccess {
            status: DatasetAccessStatus::Open,
            public_cutoff_date: None,
        }),
        categories: Vec::new(),
        dataset: "aster:BTCUSDT".into(),
    };
    let date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    let mut days = build_day_coverages(Vec::new(), &[], date, date);
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
    let remote_snapshot = SnapshotEntry {
        key: "snapshots/standard/aster/ASTERUSDT/2026-06-01.jsonl.zst".into(),
    };
    let dataset = RemoteDatasetEntry {
        source: "aster".into(),
        market: "ASTERUSDT".into(),
        start: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 6, 1, 23, 59, 59).unwrap(),
        catalog_source: Some("manifest".into()),
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
        days: build_day_coverages(vec![remote_snapshot.clone()], &[], date, date),
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
    let server = SnapshotListTestServer::spawn(
        dataset.source.clone(),
        dataset.market.clone(),
        vec![remote_snapshot],
        2048,
    )
    .await;
    let client =
        PolarisClient::new(server.base_url(), None, Duration::from_secs(1)).expect("client");

    app.pump_sync_updates(&client).await.expect("first pump");
    let sync = app.active_sync.as_ref().expect("sync still active");
    assert_eq!(sync.downloaded, 1);
    assert_eq!(sync.download_bytes, 2048);
    assert_eq!(sync.download_total_bytes, Some(2048));
    assert!(matches!(
        sync.deferred_update,
        Some(DaySyncUpdate::Finished { .. })
    ));
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
