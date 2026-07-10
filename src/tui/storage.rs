use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::error::{Result, TickError};

use super::model::{AccountIdentity, BookmarkStore, FileManagerTarget};

fn bookmarks_path(root: &Path) -> PathBuf {
    root.join("bookmarks.json")
}

fn account_identity_path(root: &Path) -> PathBuf {
    root.join("account").join("identity.json")
}

pub(crate) fn load_bookmarks(root: &Path) -> Result<BTreeSet<String>> {
    let path = bookmarks_path(root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(err) => return Err(TickError::Other(err.into())),
    };

    let store: BookmarkStore = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map_err(TickError::Other)?;
    Ok(store.bookmarks)
}

pub(crate) fn save_bookmarks(root: &Path, bookmarks: &BTreeSet<String>) -> Result<()> {
    fs::create_dir_all(root).map_err(|err| TickError::Other(err.into()))?;
    let path = bookmarks_path(root);
    let contents = serde_json::to_string_pretty(&BookmarkStore {
        bookmarks: bookmarks.clone(),
    })
    .with_context(|| format!("failed to serialize {}", path.display()))
    .map_err(TickError::Other)?;
    fs::write(&path, contents).map_err(|err| TickError::Other(err.into()))
}

pub(crate) fn clear_bookmarks(root: &Path) -> Result<()> {
    let path = bookmarks_path(root);
    if !path.exists() {
        return Ok(());
    }

    save_bookmarks(root, &BTreeSet::new())
}

pub(crate) fn load_account_identity(root: &Path) -> Result<Option<AccountIdentity>> {
    let path = account_identity_path(root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(TickError::Other(err.into())),
    };

    serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map(Some)
        .map_err(TickError::Other)
}

pub(crate) fn save_account_identity(root: &Path, identity: &AccountIdentity) -> Result<()> {
    let path = account_identity_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| TickError::Other(err.into()))?;
    }

    let contents = serde_json::to_string_pretty(identity)
        .with_context(|| format!("failed to serialize {}", path.display()))
        .map_err(TickError::Other)?;
    fs::write(&path, contents).map_err(|err| TickError::Other(err.into()))
}

pub(crate) fn snapshot_reveal_target(
    data_root: &Path,
    snapshot_paths: &[PathBuf],
) -> Option<FileManagerTarget> {
    for path in snapshot_paths {
        if path.is_file() {
            return Some(FileManagerTarget::File(path.clone()));
        }
        if path.is_dir() {
            return Some(FileManagerTarget::Directory(path.clone()));
        }

        let mut parent = path.parent();
        while let Some(dir) = parent {
            if dir.is_dir() {
                return Some(FileManagerTarget::Directory(dir.to_path_buf()));
            }
            if dir == data_root {
                break;
            }
            parent = dir.parent();
        }
    }

    if data_root.is_dir() {
        return Some(FileManagerTarget::Directory(data_root.to_path_buf()));
    }

    None
}

pub(crate) fn open_in_file_manager(target: &FileManagerTarget) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        match target {
            FileManagerTarget::File(path) => {
                command.arg("-R").arg(path);
            }
            FileManagerTarget::Directory(path) => {
                command.arg(path);
            }
        }
        command
            .spawn()
            .with_context(|| "failed to launch Finder".to_string())
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        match target {
            FileManagerTarget::File(path) => {
                command.arg(format!("/select,{}", path.display()));
            }
            FileManagerTarget::Directory(path) => {
                command.arg(path);
            }
        }
        command
            .spawn()
            .with_context(|| "failed to launch Explorer".to_string())
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let path = match target {
            FileManagerTarget::File(path) => path.parent().unwrap_or(path),
            FileManagerTarget::Directory(path) => path.as_path(),
        };
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .with_context(|| "failed to launch file manager".to_string())
            .map_err(TickError::Other)?;
        Ok(())
    }
}

pub(crate) fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .with_context(|| format!("failed to launch browser for {url}"))
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(url)
            .spawn()
            .with_context(|| format!("failed to launch browser for {url}"))
            .map_err(TickError::Other)?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .with_context(|| format!("failed to launch browser for {url}"))
            .map_err(TickError::Other)?;
        Ok(())
    }
}
