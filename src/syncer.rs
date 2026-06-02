use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use fs4::fs_std::FileExt;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc::UnboundedSender};
use tokio::time::{Duration, sleep};

use crate::api::PolarisClient;
use crate::error::{Result, TickError};
use crate::layout::Layout;
use crate::planner::{LocalSnapshotState, SnapshotPlan, SyncPlan};

const RETRY_DELAYS_MS: [u64; 5] = [500, 1000, 2000, 4000, 8000];

#[derive(Debug)]
pub struct SyncLockGuard {
    path: PathBuf,
    file: std::fs::File,
}

#[derive(Debug, Serialize)]
pub struct FailedDownload {
    pub key: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct SyncExecution {
    pub downloaded_keys: Vec<String>,
    pub failed: Vec<FailedDownload>,
}

#[derive(Debug, Clone)]
pub enum SyncProgressEvent {
    Downloaded { key: String },
    Failed { key: String, error: String },
}

impl SyncExecution {
    pub fn downloaded_total(&self) -> usize {
        self.downloaded_keys.len()
    }

    pub fn failed_total(&self) -> usize {
        self.failed.len()
    }
}

pub fn acquire_sync_lock(layout: &Layout) -> Result<SyncLockGuard> {
    let path = layout.lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))
            .map_err(TickError::Other)?;
    }

    let active = active_locks();
    {
        let mut guard = active.lock().expect("active locks");
        if guard.contains(&path) {
            return Err(TickError::LockHeld(path.clone()));
        }
        guard.insert(path.clone());
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))
        .map_err(|err| {
            active.lock().expect("active locks").remove(&path);
            TickError::Other(err)
        })?;

    file.try_lock_exclusive().map_err(|_| {
        active.lock().expect("active locks").remove(&path);
        TickError::LockHeld(path.clone())
    })?;

    Ok(SyncLockGuard { path, file })
}

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        active_locks()
            .lock()
            .expect("active locks")
            .remove(&self.path);
    }
}

fn active_locks() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static ACTIVE_LOCKS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE_LOCKS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub async fn execute_sync(
    client: &PolarisClient,
    plan: &SyncPlan,
    concurrency: usize,
) -> SyncExecution {
    execute_sync_inner(client, plan, concurrency, None).await
}

pub async fn execute_sync_with_progress(
    client: &PolarisClient,
    plan: &SyncPlan,
    concurrency: usize,
    progress: UnboundedSender<SyncProgressEvent>,
) -> SyncExecution {
    execute_sync_inner(client, plan, concurrency, Some(progress)).await
}

async fn execute_sync_inner(
    client: &PolarisClient,
    plan: &SyncPlan,
    concurrency: usize,
    progress: Option<UnboundedSender<SyncProgressEvent>>,
) -> SyncExecution {
    let semaphore = std::sync::Arc::new(Semaphore::new(concurrency));
    let mut tasks = futures_util::stream::FuturesUnordered::new();

    for snapshot in plan.missing_snapshots() {
        let client = client.clone();
        let permit = semaphore.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore closed");
            let key = snapshot.key.clone();
            match download_with_retry(&client, snapshot).await {
                Ok(()) => Ok(key),
                Err(err) => Err(FailedDownload {
                    key,
                    error: err.to_string(),
                }),
            }
        }));
    }

    let mut downloaded_keys = Vec::new();
    let mut failed = Vec::new();

    while let Some(task) = tasks.next().await {
        match task {
            Ok(Ok(key)) => {
                if let Some(progress) = &progress {
                    let _ = progress.send(SyncProgressEvent::Downloaded { key: key.clone() });
                }
                downloaded_keys.push(key);
            }
            Ok(Err(failure)) => {
                if let Some(progress) = &progress {
                    let _ = progress.send(SyncProgressEvent::Failed {
                        key: failure.key.clone(),
                        error: failure.error.clone(),
                    });
                }
                failed.push(failure);
            }
            Err(join_err) => {
                let failure = FailedDownload {
                    key: "<task>".into(),
                    error: join_err.to_string(),
                };
                if let Some(progress) = &progress {
                    let _ = progress.send(SyncProgressEvent::Failed {
                        key: failure.key.clone(),
                        error: failure.error.clone(),
                    });
                }
                failed.push(failure);
            }
        }
    }

    downloaded_keys.sort();
    failed.sort_by(|left, right| left.key.cmp(&right.key));

    SyncExecution {
        downloaded_keys,
        failed,
    }
}

async fn download_with_retry(client: &PolarisClient, snapshot: SnapshotPlan) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        match download_once(client, &snapshot).await {
            Ok(()) => return Ok(()),
            Err(err) if err.retryable() && attempt < RETRY_DELAYS_MS.len() => {
                let delay = RETRY_DELAYS_MS[attempt];
                attempt += 1;
                sleep(Duration::from_millis(delay)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn download_once(client: &PolarisClient, snapshot: &SnapshotPlan) -> Result<()> {
    if let Some(parent) = snapshot.local_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))
            .map_err(TickError::Other)?;
    }
    if let Some(parent) = snapshot.temp_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))
            .map_err(TickError::Other)?;
    }

    if tokio::fs::metadata(&snapshot.temp_path).await.is_ok() {
        tokio::fs::remove_file(&snapshot.temp_path)
            .await
            .with_context(|| format!("failed to remove {}", snapshot.temp_path.display()))
            .map_err(TickError::Other)?;
    }

    let response = client.download(&snapshot.download_url).await?;

    if tokio::fs::metadata(&snapshot.local_path).await.is_ok()
        && snapshot.state != LocalSnapshotState::Present
    {
        tokio::fs::remove_file(&snapshot.local_path)
            .await
            .with_context(|| format!("failed to remove {}", snapshot.local_path.display()))
            .map_err(TickError::Other)?;
    }

    let mut file = tokio::fs::File::create(&snapshot.temp_path)
        .await
        .with_context(|| format!("failed to create {}", snapshot.temp_path.display()))
        .map_err(TickError::Other)?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .with_context(|| format!("failed to stream {}", snapshot.key))
            .map_err(TickError::Other)?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .with_context(|| format!("failed to write {}", snapshot.temp_path.display()))
            .map_err(TickError::Other)?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .with_context(|| format!("failed to flush {}", snapshot.temp_path.display()))
        .map_err(TickError::Other)?;
    file.sync_all()
        .await
        .with_context(|| format!("failed to sync {}", snapshot.temp_path.display()))
        .map_err(TickError::Other)?;
    drop(file);

    if tokio::fs::metadata(&snapshot.local_path).await.is_ok() {
        tokio::fs::remove_file(&snapshot.local_path)
            .await
            .with_context(|| format!("failed to replace {}", snapshot.local_path.display()))
            .map_err(TickError::Other)?;
    }

    tokio::fs::rename(&snapshot.temp_path, &snapshot.local_path)
        .await
        .with_context(|| {
            format!(
                "failed to move {} into place",
                snapshot.local_path.display()
            )
        })
        .map_err(TickError::Other)?;

    Ok(())
}

pub fn layout_for_root(root: PathBuf) -> Layout {
    Layout::new(root)
}
