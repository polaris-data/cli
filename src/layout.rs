use std::path::{Path, PathBuf};

use anyhow::anyhow;
use chrono::{DateTime, NaiveDate, Utc};
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
    pub source: Option<String>,
    pub market: Option<String>,
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
        let (tier, source, market, date) = parse_opaque_key(key)?;
        let mut path = self.root.join("data");
        path.push(tier);
        path.push(source);
        path.push(market);
        path.push(date);
        path.push(format!("{key}.jsonl.zst"));
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

    pub fn catalog_cache_path(&self, source: &str, market: &str) -> PathBuf {
        self.cache_root()
            .join("catalog")
            .join(source)
            .join(format!("{market}.json"))
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

fn parse_opaque_key(key: &str) -> Result<(&str, &str, &str, &str)> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(TickError::InvalidArgument(
            "snapshot key must not be empty".into(),
        ));
    }

    let (first_digit, _) = trimmed
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_digit())
        .ok_or_else(|| {
            TickError::InvalidArgument(format!("opaque key does not contain a date: {key}"))
        })?;

    if first_digit == 0 || trimmed.as_bytes()[first_digit - 1] != b'-' {
        return Err(TickError::InvalidArgument(format!(
            "opaque key has unexpected format: {key}"
        )));
    }

    let end = first_digit + 10;
    if trimmed.len() < end {
        return Err(TickError::InvalidArgument(format!(
            "opaque key too short for date: {key}"
        )));
    }

    let date_str = &trimmed[first_digit..end];
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map_err(|_| {
            TickError::InvalidArgument(format!("invalid date in opaque key: {key}"))
        })?;

    let prefix = &trimmed[..first_digit - 1];
    let first_dash = prefix
        .find('-')
        .ok_or_else(|| {
            TickError::InvalidArgument(format!("invalid opaque key prefix: {key}"))
        })?;
    let last_dash = prefix
        .rfind('-')
        .ok_or_else(|| {
            TickError::InvalidArgument(format!("invalid opaque key prefix: {key}"))
        })?;

    let tier = &prefix[..first_dash];
    let source = &prefix[first_dash + 1..last_dash];
    let market = &prefix[last_dash + 1..];
    Ok((tier, source, market, date_str))
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
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        let filename = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        let key = filename
            .strip_suffix(".jsonl.zst")
            .unwrap_or(&filename)
            .to_string();
        let (source, market, date) = infer_local_metadata(&relative_str);

        files.push(LocalSnapshotEntry {
            key,
            path: path.to_string_lossy().to_string(),
            filename,
            source,
            market,
            date,
            start: None,
            end: None,
        });
    }

    Ok(())
}

pub fn infer_date_from_text(text: &str) -> Option<chrono::NaiveDate> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(9) {
        if bytes[i].is_ascii_digit()
            && bytes[i + 4] == b'-'
            && bytes[i + 7] == b'-'
            && bytes[i + 5].is_ascii_digit()
            && bytes[i + 6].is_ascii_digit()
            && bytes[i + 8].is_ascii_digit()
            && bytes[i + 9].is_ascii_digit()
        {
            let candidate = &text[i..i + 10];
            if let Ok(date) = NaiveDate::parse_from_str(candidate, "%Y-%m-%d") {
                return Some(date);
            }
        }
    }
    None
}

fn infer_local_metadata(
    relative_path: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let segments: Vec<&str> = relative_path.split('/').collect();
    if segments.len() >= 5 {
        let source = Some(segments[1].to_string());
        let market = Some(segments[2].to_string());
        let date = Some(segments[3].to_string());
        (source, market, date)
    } else {
        (None, None, None)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Layout, infer_date_from_text};

    #[test]
    fn opaque_key_maps_to_canonical_path() {
        let layout = Layout::new(PathBuf::from("/tmp/tick"));
        let path = layout
            .data_path_for_key("standard-aster-ASTERUSDT-2026-06-01-00")
            .expect("path");
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/tick/data/standard/aster/ASTERUSDT/2026-06-01/standard-aster-ASTERUSDT-2026-06-01-00.jsonl.zst"
            )
        );
    }

    #[test]
    fn local_listing_reads_metadata_from_directory_structure() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        let layout = Layout::new(tempdir.path().to_path_buf());
        let file = layout
            .data_path_for_key("standard-aster-ASTERUSDT-2026-06-01-00")
            .expect("path");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, b"snapshot").expect("write");

        let entries = layout.list_local_snapshots().expect("entries");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.key, "standard-aster-ASTERUSDT-2026-06-01-00");
        assert_eq!(entry.source.as_deref(), Some("aster"));
        assert_eq!(entry.market.as_deref(), Some("ASTERUSDT"));
        assert_eq!(entry.date.as_deref(), Some("2026-06-01"));
    }

    #[test]
    fn date_extraction_finds_yyyy_mm_dd_in_opaque_key() {
        assert_eq!(
            infer_date_from_text("standard-aster-ASTERUSDT-2026-06-01-00"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap())
        );
        assert_eq!(
            infer_date_from_text("raw-aster-ASTERUSDT-2026-06-02-000000"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 2).unwrap())
        );
    }
}
