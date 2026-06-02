use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::api::{CatalogResponse, PolarisClient, SnapshotEntry};
use crate::config::Config;
use crate::error::{Result, TickError};
use crate::layout::Layout;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TimeWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SnapshotPlan {
    pub key: String,
    pub local_path: PathBuf,
    pub temp_path: PathBuf,
    pub local_size: u64,
    pub state: LocalSnapshotState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSnapshotState {
    Present,
    Missing,
    Incomplete,
}

#[derive(Debug, Clone)]
pub struct SyncPlan {
    pub exchange: String,
    pub asset: String,
    pub requested_range: TimeWindow,
    pub effective_range: TimeWindow,
    pub root: PathBuf,
    pub total_remote_bytes: u64,
    pub snapshots: Vec<SnapshotPlan>,
}

impl SyncPlan {
    pub fn remote_total(&self) -> usize {
        self.snapshots.len()
    }

    pub fn present_total(&self) -> usize {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.state == LocalSnapshotState::Present)
            .count()
    }

    pub fn missing_total(&self) -> usize {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.state != LocalSnapshotState::Present)
            .count()
    }

    pub fn present_bytes(&self) -> u64 {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.state == LocalSnapshotState::Present)
            .map(|snapshot| snapshot.local_size)
            .sum()
    }

    pub fn missing_bytes(&self) -> u64 {
        self.total_remote_bytes.saturating_sub(self.present_bytes())
    }

    pub fn missing_keys(&self) -> Vec<String> {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.state != LocalSnapshotState::Present)
            .map(|snapshot| snapshot.key.clone())
            .collect()
    }

    pub fn missing_snapshots(&self) -> Vec<SnapshotPlan> {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.state != LocalSnapshotState::Present)
            .cloned()
            .collect()
    }
}

pub async fn build_sync_plan(
    client: &PolarisClient,
    config: &Config,
    exchange: &str,
    asset: &str,
    requested_range: TimeWindow,
) -> Result<SyncPlan> {
    let layout = Layout::new(config.root.clone());
    let catalog = client.fetch_catalog(Some(exchange), Some(asset)).await?;
    let coverage = find_asset_coverage(&catalog, exchange, asset).ok_or_else(|| {
        TickError::DatasetUnavailable(format!("dataset {exchange}/{asset} is not available"))
    })?;
    let effective_range = intersect_ranges(
        &requested_range,
        &TimeWindow {
            from: coverage.start,
            to: coverage.end,
        },
    )
    .ok_or_else(|| {
        TickError::DatasetUnavailable(format!(
            "requested range does not overlap remote coverage for {exchange}/{asset}"
        ))
    })?;

    let (remote_snapshots, total_remote_bytes) = client
        .list_snapshots(exchange, asset, effective_range.from, effective_range.to)
        .await?;

    let snapshots = classify_snapshots(&layout, remote_snapshots).await?;

    Ok(SyncPlan {
        exchange: exchange.to_string(),
        asset: asset.to_string(),
        requested_range,
        effective_range,
        root: config.root.clone(),
        total_remote_bytes,
        snapshots,
    })
}

async fn classify_snapshots(
    layout: &Layout,
    remote_snapshots: Vec<SnapshotEntry>,
) -> Result<Vec<SnapshotPlan>> {
    let mut snapshots = Vec::with_capacity(remote_snapshots.len());
    for snapshot in remote_snapshots {
        let local_path = layout.data_path_for_key(&snapshot.key)?;
        let temp_path = layout.temp_path_for_key(&snapshot.key);
        let temp_exists = tokio::fs::metadata(&temp_path).await.is_ok();
        let metadata = tokio::fs::metadata(&local_path).await.ok();

        let (state, local_size) = match (temp_exists, metadata) {
            (true, _) => (LocalSnapshotState::Incomplete, 0),
            (false, Some(metadata)) if metadata.len() > 0 => {
                (LocalSnapshotState::Present, metadata.len())
            }
            _ => (LocalSnapshotState::Missing, 0),
        };

        snapshots.push(SnapshotPlan {
            key: snapshot.key,
            local_path,
            temp_path,
            local_size,
            state,
        });
    }
    Ok(snapshots)
}

fn find_asset_coverage<'a>(
    catalog: &'a CatalogResponse,
    exchange: &str,
    asset: &str,
) -> Option<&'a crate::api::CatalogAsset> {
    catalog
        .exchanges
        .iter()
        .find(|entry| entry.id == exchange)
        .and_then(|entry| entry.assets.iter().find(|candidate| candidate.id == asset))
}

pub fn intersect_ranges(requested: &TimeWindow, available: &TimeWindow) -> Option<TimeWindow> {
    let from = requested.from.max(available.from);
    let to = requested.to.min(available.to);
    if from > to {
        return None;
    }
    Some(TimeWindow { from, to })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;

    use super::{LocalSnapshotState, SnapshotPlan, TimeWindow, intersect_ranges};
    use crate::layout::Layout;

    #[test]
    fn coverage_intersection_handles_overlap_and_gaps() {
        let requested = TimeWindow {
            from: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        };
        let available = TimeWindow {
            from: Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 6, 3, 0, 0, 0).unwrap(),
        };
        let intersection = intersect_ranges(&requested, &available).expect("intersection");
        assert_eq!(intersection.from, available.from);
        assert_eq!(intersection.to, requested.to);

        let missing = TimeWindow {
            from: Utc.with_ymd_and_hms(2026, 6, 3, 0, 0, 0).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 6, 4, 0, 0, 0).unwrap(),
        };
        assert!(intersect_ranges(&requested, &missing).is_none());
    }

    #[tokio::test]
    async fn diff_classifies_present_missing_and_incomplete() {
        let root = TempDir::new().expect("tempdir");
        let layout = Layout::new(root.path().to_path_buf());

        let present = layout
            .data_path_for_key("bronze/ex/asset/2026-01-01/present.jsonl.zst")
            .expect("present path");
        let incomplete =
            layout.temp_path_for_key("bronze/ex/asset/2026-01-01/incomplete.jsonl.zst");

        tokio::fs::create_dir_all(present.parent().expect("present parent"))
            .await
            .expect("mkdir");
        tokio::fs::write(&present, b"ok")
            .await
            .expect("write present");
        tokio::fs::create_dir_all(incomplete.parent().expect("tmp parent"))
            .await
            .expect("mkdir tmp");
        tokio::fs::write(&incomplete, b"partial")
            .await
            .expect("write tmp");

        let snapshots = super::classify_snapshots(
            &layout,
            vec![
                crate::api::SnapshotEntry {
                    key: "bronze/ex/asset/2026-01-01/present.jsonl.zst".into(),
                    filename: "present.jsonl.zst".into(),
                },
                crate::api::SnapshotEntry {
                    key: "bronze/ex/asset/2026-01-01/missing.jsonl.zst".into(),
                    filename: "missing.jsonl.zst".into(),
                },
                crate::api::SnapshotEntry {
                    key: "bronze/ex/asset/2026-01-01/incomplete.jsonl.zst".into(),
                    filename: "incomplete.jsonl.zst".into(),
                },
            ],
        )
        .await
        .expect("classified");

        let states: Vec<LocalSnapshotState> = snapshots.iter().map(|item| item.state).collect();
        assert_eq!(
            states,
            vec![
                LocalSnapshotState::Present,
                LocalSnapshotState::Missing,
                LocalSnapshotState::Incomplete,
            ]
        );
    }

    #[test]
    fn sync_plan_summaries_are_consistent() {
        let snapshots = vec![
            SnapshotPlan {
                key: "a".into(),
                local_path: PathBuf::from("/tmp/a"),
                temp_path: PathBuf::from("/tmp/a.part"),
                local_size: 5,
                state: LocalSnapshotState::Present,
            },
            SnapshotPlan {
                key: "b".into(),
                local_path: PathBuf::from("/tmp/b"),
                temp_path: PathBuf::from("/tmp/b.part"),
                local_size: 0,
                state: LocalSnapshotState::Missing,
            },
        ];
        let plan = super::SyncPlan {
            exchange: "ex".into(),
            asset: "asset".into(),
            requested_range: TimeWindow {
                from: Utc::now(),
                to: Utc::now(),
            },
            effective_range: TimeWindow {
                from: Utc::now(),
                to: Utc::now(),
            },
            root: PathBuf::from("/tmp"),
            total_remote_bytes: 10,
            snapshots,
        };
        assert_eq!(plan.remote_total(), 2);
        assert_eq!(plan.present_total(), 1);
        assert_eq!(plan.missing_total(), 1);
        assert_eq!(plan.missing_bytes(), 5);
    }
}
