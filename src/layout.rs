use std::path::{Path, PathBuf};

use anyhow::anyhow;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Result, TickError};

#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalSnapshotEntry {
    pub key: String,
    pub path: String,
    pub filename: String,
    pub exchange: Option<String>,
    pub asset: Option<String>,
    pub date: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

impl Layout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn data_path_for_key(&self, key: &str) -> Result<PathBuf> {
        let segments = validated_key_segments(key)?;
        let mut path = self.root.join("data");
        for segment in segments {
            path.push(segment);
        }
        Ok(path)
    }

    pub fn temp_path_for_key(&self, key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        self.tmp_root().join(format!("{digest}.part"))
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join("locks").join("sync.lock")
    }

    pub fn cache_root(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn catalog_cache_path(&self, exchange: &str, asset: &str) -> PathBuf {
        self.cache_root()
            .join("catalog")
            .join(exchange)
            .join(format!("{asset}.json"))
    }

    pub fn data_root(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn tmp_root(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn list_local_snapshots(&self) -> Result<Vec<LocalSnapshotEntry>> {
        let data_root = self.data_root();
        let mut files = Vec::new();
        if !data_root.exists() {
            return Ok(files);
        }

        collect_snapshot_files(&data_root, &data_root, &mut files)?;
        files.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(files)
    }
}

fn validated_key_segments(key: &str) -> Result<Vec<&str>> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(TickError::InvalidArgument(
            "snapshot key must not be empty".into(),
        ));
    }

    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(TickError::Other(anyhow!(
                "invalid remote key segment in {trimmed}"
            )));
        }
        if segment.contains('\\') {
            return Err(TickError::Other(anyhow!(
                "invalid remote key segment in {trimmed}"
            )));
        }
        segments.push(segment);
    }
    Ok(segments)
}

fn collect_snapshot_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<LocalSnapshotEntry>,
) -> Result<()> {
    for entry in std::fs::read_dir(current).map_err(|err| {
        TickError::Other(anyhow!(err).context(format!("failed to read {}", current.display())))
    })? {
        let entry = entry.map_err(|err| {
            TickError::Other(
                anyhow!(err).context(format!("failed to read entry in {}", current.display())),
            )
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|err| {
            TickError::Other(anyhow!(err).context(format!("failed to stat {}", path.display())))
        })?;

        if file_type.is_dir() {
            collect_snapshot_files(root, &path, files)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        let relative = match path.strip_prefix(root) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let key = relative.to_string_lossy().replace('\\', "/");
        let filename = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        let (exchange, asset, date) = infer_snapshot_identity(&key, &filename);
        let (start, end) = parse_snapshot_times(&filename);

        files.push(LocalSnapshotEntry {
            key,
            path: path.to_string_lossy().to_string(),
            filename,
            exchange,
            asset,
            date,
            start,
            end,
        });
    }

    Ok(())
}

pub fn infer_snapshot_date_from_key(key: &str) -> Option<NaiveDate> {
    let segments = key.split('/').collect::<Vec<_>>();
    infer_date_from_segments(&segments).or_else(|| {
        segments
            .last()
            .and_then(|filename| infer_date_from_text(filename))
    })
}

fn parse_snapshot_times(filename: &str) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let start = filename
        .split("_s")
        .nth(1)
        .and_then(|value| value.split("_e").next())
        .and_then(parse_snapshot_timestamp);
    let end = filename
        .split("_e")
        .nth(1)
        .and_then(|value| value.split('.').next())
        .and_then(parse_snapshot_timestamp);
    (start, end)
}

fn parse_snapshot_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%SZ").ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn infer_snapshot_identity(
    key: &str,
    filename: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let segments = key.split('/').collect::<Vec<_>>();
    if segments.is_empty() {
        return (None, None, None);
    }

    if let Some((index, date)) = infer_date_segment_index(&segments) {
        let exchange = index
            .checked_sub(2)
            .and_then(|value| segments.get(value))
            .map(|value| (*value).to_string());
        let asset = index
            .checked_sub(1)
            .and_then(|value| segments.get(value))
            .map(|value| (*value).to_string());
        return (exchange, asset, Some(date.to_string()));
    }

    if let Some(date) = infer_date_from_text(filename) {
        let exchange = segments
            .len()
            .checked_sub(3)
            .and_then(|value| segments.get(value))
            .map(|value| (*value).to_string());
        let asset = segments
            .len()
            .checked_sub(2)
            .and_then(|value| segments.get(value))
            .map(|value| (*value).to_string());
        return (exchange, asset, Some(date.to_string()));
    }

    let exchange = segments
        .len()
        .checked_sub(4)
        .and_then(|value| segments.get(value))
        .map(|value| (*value).to_string());
    let asset = segments
        .len()
        .checked_sub(3)
        .and_then(|value| segments.get(value))
        .map(|value| (*value).to_string());
    (exchange, asset, None)
}

fn infer_date_from_segments(segments: &[&str]) -> Option<NaiveDate> {
    infer_date_segment_index(segments).map(|(_, date)| date)
}

fn infer_date_segment_index(segments: &[&str]) -> Option<(usize, NaiveDate)> {
    segments
        .iter()
        .enumerate()
        .find_map(|(index, segment)| infer_date_from_text(segment).map(|date| (index, date)))
}

fn infer_date_from_text(text: &str) -> Option<NaiveDate> {
    text.split(|ch: char| !(ch.is_ascii_digit() || ch == '-'))
        .find_map(|token| {
            if token.len() == 10 {
                NaiveDate::parse_from_str(token, "%Y-%m-%d").ok()
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{TimeZone, Utc};

    use super::{Layout, infer_snapshot_date_from_key};

    #[test]
    fn remote_key_maps_to_canonical_path() {
        let layout = Layout::new(PathBuf::from("/tmp/tick"));
        let path = layout
            .data_path_for_key("bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst")
            .expect("path");
        assert_eq!(
            path,
            PathBuf::from("/tmp/tick/data/bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst")
        );
    }

    #[test]
    fn local_listing_deduces_snapshot_metadata_from_filename() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let layout = Layout::new(tempdir.path().to_path_buf());
        let file = layout
            .data_path_for_key(
                "bronze/aster/BTCUSDT/2026-06-01/aster_BTCUSDT_s20260601T000000Z_e20260601T000959Z.jsonl.zst",
            )
            .expect("path");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, b"snapshot").expect("write");

        let entries = layout.list_local_snapshots().expect("entries");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.exchange.as_deref(), Some("aster"));
        assert_eq!(entry.asset.as_deref(), Some("BTCUSDT"));
        assert_eq!(entry.date.as_deref(), Some("2026-06-01"));
        assert_eq!(
            entry.start,
            Some(Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(
            entry.end,
            Some(Utc.with_ymd_and_hms(2026, 6, 1, 0, 9, 59).unwrap())
        );
    }

    #[test]
    fn local_listing_infers_metadata_from_daily_snapshot_filename() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let layout = Layout::new(tempdir.path().to_path_buf());
        let file = layout
            .data_path_for_key("events/aster/BTCUSDT/aster_BTCUSDT_2026-06-01.jsonl.zst")
            .expect("path");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, b"snapshot").expect("write");

        let entries = layout.list_local_snapshots().expect("entries");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.exchange.as_deref(), Some("aster"));
        assert_eq!(entry.asset.as_deref(), Some("BTCUSDT"));
        assert_eq!(entry.date.as_deref(), Some("2026-06-01"));
        assert_eq!(entry.start, None);
        assert_eq!(entry.end, None);
    }

    #[test]
    fn snapshot_date_inference_supports_directory_and_filename_dates() {
        assert_eq!(
            infer_snapshot_date_from_key("bronze/aster/BTCUSDT/2026-06-01/file.jsonl.zst"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap())
        );
        assert_eq!(
            infer_snapshot_date_from_key("events/aster/BTCUSDT/aster_BTCUSDT_2026-06-02.jsonl.zst"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 2).unwrap())
        );
    }
}
