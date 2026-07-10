use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration as ChronoDuration, NaiveDate};
use ratatui::style::Color;

use crate::api::{DatasetAccess, DatasetAccessStatus, SnapshotEntry};
use crate::layout::{LocalSnapshotEntry, infer_date_from_text};

use super::model::{
    ActiveDaySync, ApiKeyRequirement, BrowserCategory, DatasetView, DayCoverage, DayState,
    LocalDatasetSummary, RemoteDatasetEntry,
};

pub(crate) fn summarize_local_snapshots(
    snapshots: &[LocalSnapshotEntry],
) -> BTreeMap<String, LocalDatasetSummary> {
    let mut summaries = BTreeMap::new();
    for snapshot in snapshots {
        let (Some(source), Some(market)) = (snapshot.source.as_deref(), snapshot.market.as_deref())
        else {
            continue;
        };
        let key = format!("{source}:{market}");
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

pub(crate) fn group_local_snapshot_keys(
    snapshots: Vec<LocalSnapshotEntry>,
) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::new();
    for snapshot in snapshots {
        let (Some(source), Some(market)) = (snapshot.source.clone(), snapshot.market.clone())
        else {
            continue;
        };
        grouped
            .entry(format!("{source}:{market}"))
            .or_insert_with(Vec::new)
            .push(snapshot.key);
    }
    for keys in grouped.values_mut() {
        keys.sort();
    }
    grouped
}

pub(crate) fn diff_missing_snapshot_keys(
    remote_keys: Vec<String>,
    local_keys: &[String],
) -> Vec<String> {
    let local_set = local_keys.iter().collect::<BTreeSet<_>>();
    remote_keys
        .into_iter()
        .filter(|key| !local_set.contains(key))
        .collect()
}

pub(crate) fn build_day_coverages(
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
        if let Some(date) = snapshot.date {
            if date >= start_date && date <= end_date {
                let offset = (date - start_date).num_days() as usize;
                days[offset].remote_keys.push(snapshot.key);
            }
        }
    }

    for key in local_keys {
        if let Some(date) = infer_date_from_text(key) {
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

pub(crate) fn select_initial_day(days: &[DayCoverage]) -> usize {
    days.iter()
        .position(|day| !day.missing_keys.is_empty())
        .unwrap_or(0)
}

pub(crate) fn compact_count(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

pub(crate) fn format_byte_progress(downloaded_bytes: u64, total_bytes: Option<u64>) -> String {
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

pub(crate) fn format_bytes(bytes: u64) -> String {
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

pub(crate) fn spinner_frame(tick: usize) -> &'static str {
    match tick % 4 {
        0 => "|",
        1 => "/",
        2 => "-",
        _ => "\\",
    }
}

pub(crate) fn sync_adjusted_day_totals<'a>(
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

pub(crate) fn render_completion_bar(
    local_total: usize,
    remote_total: usize,
    width: usize,
) -> String {
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

pub(crate) fn access_color(access: Option<&DatasetAccess>) -> Color {
    match access.map(|item| &item.status) {
        Some(DatasetAccessStatus::Open) => Color::Green,
        Some(DatasetAccessStatus::Preview) => Color::Yellow,
        Some(DatasetAccessStatus::Restricted) => Color::Red,
        None => Color::DarkGray,
    }
}

pub(crate) fn browser_categories(datasets: &[RemoteDatasetEntry]) -> Vec<BrowserCategory> {
    let mut categories = vec![BrowserCategory::AllDatasets, BrowserCategory::Bookmarks];
    let catalog_categories = datasets
        .iter()
        .flat_map(|dataset| dataset.categories.iter().cloned())
        .collect::<BTreeSet<_>>();
    categories.extend(catalog_categories.into_iter().map(BrowserCategory::Catalog));
    categories
}

pub(crate) fn api_key_requirement_for_download(
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
