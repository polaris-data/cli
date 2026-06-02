use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Context;
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::api::PolarisClient;
use crate::error::{Result, TickError};
use crate::layout::{Layout, LocalSnapshotEntry};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum MaterializeDayStatus {
    Built,
    Incomplete,
    NoRemoteData,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MaterializedDay {
    pub date: String,
    pub status: MaterializeDayStatus,
    pub remote_total: usize,
    pub local_total: usize,
    pub missing_total: usize,
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MaterializeExecution {
    pub exchange: String,
    pub asset: String,
    pub built_total: usize,
    pub incomplete_total: usize,
    pub no_remote_total: usize,
    pub days: Vec<MaterializedDay>,
}

pub async fn materialize_days(
    client: &PolarisClient,
    layout: &Layout,
    exchange: &str,
    asset: &str,
    date_filter: Option<NaiveDate>,
) -> Result<MaterializeExecution> {
    let local_snapshots = layout.list_local_snapshots()?;
    let local_entries = local_snapshots
        .into_iter()
        .filter(|entry| {
            entry.exchange.as_deref() == Some(exchange) && entry.asset.as_deref() == Some(asset)
        })
        .collect::<Vec<_>>();

    let dates = collect_candidate_dates(&local_entries, date_filter);
    materialize_specific_dates(client, layout, exchange, asset, &local_entries, dates).await
}

pub async fn materialize_range_days(
    client: &PolarisClient,
    layout: &Layout,
    exchange: &str,
    asset: &str,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<MaterializeExecution> {
    let local_snapshots = layout.list_local_snapshots()?;
    let local_entries = local_snapshots
        .into_iter()
        .filter(|entry| {
            entry.exchange.as_deref() == Some(exchange) && entry.asset.as_deref() == Some(asset)
        })
        .collect::<Vec<_>>();

    let dates = (0..=(end_date - start_date).num_days())
        .map(|offset| start_date + chrono::Duration::days(offset))
        .collect::<Vec<_>>();

    materialize_specific_dates(client, layout, exchange, asset, &local_entries, dates).await
}

fn collect_candidate_dates(
    local_entries: &[LocalSnapshotEntry],
    date_filter: Option<NaiveDate>,
) -> Vec<NaiveDate> {
    if let Some(date) = date_filter {
        return vec![date];
    }

    let mut dates = local_entries
        .iter()
        .filter_map(|entry| entry.date.as_deref())
        .filter_map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    dates.sort();
    dates
}

async fn materialize_specific_dates(
    client: &PolarisClient,
    layout: &Layout,
    exchange: &str,
    asset: &str,
    local_entries: &[LocalSnapshotEntry],
    dates: Vec<NaiveDate>,
) -> Result<MaterializeExecution> {
    let mut days = Vec::new();

    for date in dates {
        let from = Utc
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .single()
            .expect("valid midnight");
        let to = Utc
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 23, 59, 59)
            .single()
            .expect("valid end of day");

        let (remote_snapshots, _) = client.list_snapshots(exchange, asset, from, to).await?;
        let remote_keys = remote_snapshots
            .iter()
            .map(|snapshot| snapshot.key.clone())
            .collect::<Vec<_>>();
        let remote_total = remote_keys.len();
        let local_for_day = local_entries
            .iter()
            .filter(|entry| entry.date.as_deref() == Some(&date.to_string()))
            .collect::<Vec<_>>();
        let local_total = local_for_day.len();
        let local_key_to_path = local_for_day
            .iter()
            .map(|entry| (entry.key.clone(), PathBuf::from(&entry.path)))
            .collect::<BTreeMap<_, _>>();
        let local_key_set = local_key_to_path.keys().cloned().collect::<BTreeSet<_>>();
        let missing_keys = remote_keys
            .iter()
            .filter(|key| !local_key_set.contains(*key))
            .cloned()
            .collect::<Vec<_>>();

        if remote_total == 0 {
            days.push(MaterializedDay {
                date: date.to_string(),
                status: MaterializeDayStatus::NoRemoteData,
                remote_total,
                local_total,
                missing_total: 0,
                output_path: None,
            });
            continue;
        }

        if !missing_keys.is_empty() {
            days.push(MaterializedDay {
                date: date.to_string(),
                status: MaterializeDayStatus::Incomplete,
                remote_total,
                local_total,
                missing_total: missing_keys.len(),
                output_path: None,
            });
            continue;
        }

        let output_path = materialize_day_file(
            layout,
            exchange,
            asset,
            date,
            &remote_keys,
            &local_key_to_path,
        )
        .await?;
        days.push(MaterializedDay {
            date: date.to_string(),
            status: MaterializeDayStatus::Built,
            remote_total,
            local_total,
            missing_total: 0,
            output_path: Some(output_path.to_string_lossy().to_string()),
        });
    }

    let built_total = days
        .iter()
        .filter(|day| day.status == MaterializeDayStatus::Built)
        .count();
    let incomplete_total = days
        .iter()
        .filter(|day| day.status == MaterializeDayStatus::Incomplete)
        .count();
    let no_remote_total = days
        .iter()
        .filter(|day| day.status == MaterializeDayStatus::NoRemoteData)
        .count();

    Ok(MaterializeExecution {
        exchange: exchange.to_string(),
        asset: asset.to_string(),
        built_total,
        incomplete_total,
        no_remote_total,
        days,
    })
}

async fn materialize_day_file(
    layout: &Layout,
    exchange: &str,
    asset: &str,
    date: NaiveDate,
    ordered_keys: &[String],
    local_key_to_path: &BTreeMap<String, PathBuf>,
) -> Result<PathBuf> {
    let output_path = layout.daily_path_for_dataset_day(exchange, asset, date);
    let temp_path = layout.daily_temp_path_for_dataset_day(exchange, asset, date);

    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))
            .map_err(TickError::Other)?;
    }
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))
            .map_err(TickError::Other)?;
    }
    if tokio::fs::metadata(&temp_path).await.is_ok() {
        tokio::fs::remove_file(&temp_path)
            .await
            .with_context(|| format!("failed to remove {}", temp_path.display()))
            .map_err(TickError::Other)?;
    }

    let mut output = tokio::fs::File::create(&temp_path)
        .await
        .with_context(|| format!("failed to create {}", temp_path.display()))
        .map_err(TickError::Other)?;

    for key in ordered_keys {
        let input_path = local_key_to_path.get(key).ok_or_else(|| {
            TickError::Other(anyhow::anyhow!("missing local input path for {}", key))
        })?;
        let mut input = tokio::fs::File::open(input_path)
            .await
            .with_context(|| format!("failed to open {}", input_path.display()))
            .map_err(TickError::Other)?;
        tokio::io::copy(&mut input, &mut output)
            .await
            .with_context(|| format!("failed to append {}", input_path.display()))
            .map_err(TickError::Other)?;
    }

    tokio::io::AsyncWriteExt::flush(&mut output)
        .await
        .with_context(|| format!("failed to flush {}", temp_path.display()))
        .map_err(TickError::Other)?;
    output
        .sync_all()
        .await
        .with_context(|| format!("failed to sync {}", temp_path.display()))
        .map_err(TickError::Other)?;
    drop(output);

    if tokio::fs::metadata(&output_path).await.is_ok() {
        tokio::fs::remove_file(&output_path)
            .await
            .with_context(|| format!("failed to replace {}", output_path.display()))
            .map_err(TickError::Other)?;
    }

    tokio::fs::rename(&temp_path, &output_path)
        .await
        .with_context(|| format!("failed to move {} into place", output_path.display()))
        .map_err(TickError::Other)?;

    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::collect_candidate_dates;
    use crate::layout::LocalSnapshotEntry;

    #[test]
    fn candidate_dates_respect_filter_or_local_days() {
        let local = vec![
            LocalSnapshotEntry {
                key: "a".into(),
                path: "/tmp/a".into(),
                filename: "a".into(),
                exchange: Some("ex".into()),
                asset: Some("asset".into()),
                date: Some("2026-06-02".into()),
                start: None,
                end: None,
            },
            LocalSnapshotEntry {
                key: "b".into(),
                path: "/tmp/b".into(),
                filename: "b".into(),
                exchange: Some("ex".into()),
                asset: Some("asset".into()),
                date: Some("2026-06-01".into()),
                start: None,
                end: None,
            },
        ];

        let inferred = collect_candidate_dates(&local, None);
        assert_eq!(
            inferred,
            vec![
                NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 6, 2).unwrap()
            ]
        );

        let filtered =
            collect_candidate_dates(&local, Some(NaiveDate::from_ymd_opt(2026, 6, 3).unwrap()));
        assert_eq!(filtered, vec![NaiveDate::from_ymd_opt(2026, 6, 3).unwrap()]);
    }
}
