import fs from 'node:fs/promises'
import path from 'node:path'

import { Layout } from './layout.js'
import { lockHeld, otherError } from './errors.js'
import { inferDateFromText } from './layout.js'
import { missingSnapshots } from './planner.js'
import type { PolarisClient } from './api.js'
import type { FailedDownload, SnapshotPlan, SyncExecution, SyncPlan, SyncProgressEvent } from './types.js'

const RETRY_DELAYS_MS = [500, 1000, 2000, 4000, 8000]
const activeLocks = new Set<string>()

export class SyncLockGuard {
  constructor(readonly path: string, readonly handle: fs.FileHandle) {}

  async release(): Promise<void> {
    activeLocks.delete(this.path)
    await this.handle.close()
    await fs.rm(this.path, { force: true })
  }
}

export function layoutForRoot(root: string): Layout {
  return new Layout(root)
}

export async function acquireSyncLock(layout: Layout): Promise<SyncLockGuard> {
  const lockPath = layout.lockPath()
  await fs.mkdir(path.dirname(lockPath), { recursive: true })
  if (activeLocks.has(lockPath)) throw lockHeld(lockPath)

  activeLocks.add(lockPath)
  try {
    const handle = await fs.open(lockPath, 'wx+')
    return new SyncLockGuard(lockPath, handle)
  } catch (error) {
    activeLocks.delete(lockPath)
    const err = error as NodeJS.ErrnoException
    if (err.code === 'EEXIST') throw lockHeld(lockPath)
    throw otherError(`failed to open ${lockPath}`, error)
  }
}

export async function executeSync(
  client: PolarisClient,
  plan: SyncPlan,
  concurrency: number,
  onProgress?: (event: SyncProgressEvent) => void,
): Promise<SyncExecution> {
  const pending = await resolveDownloadTargets(client, plan)
  const downloadedKeys: string[] = []
  const failed: FailedDownload[] = []

  const workers = Array.from({ length: Math.max(1, concurrency) }, async () => {
    while (pending.length > 0) {
      const target = pending.shift()
      if (!target) break
      try {
        const totalBytes = await downloadWithRetry(client, target, onProgress)
        onProgress?.({ type: 'downloaded', key: target.snapshot.key, totalBytes })
        downloadedKeys.push(target.snapshot.key)
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error)
        onProgress?.({ type: 'failed', key: target.snapshot.key, error: message })
        failed.push({ key: target.snapshot.key, error: message })
      }
    }
  })

  await Promise.all(workers)
  downloadedKeys.sort()
  failed.sort((left, right) => left.key.localeCompare(right.key))
  return { downloadedKeys, failed }
}

type DownloadTarget = {
  snapshot: SnapshotPlan
  directUrl?: string
}

async function downloadWithRetry(
  client: PolarisClient,
  target: DownloadTarget,
  onProgress?: (event: SyncProgressEvent) => void,
): Promise<number> {
  let attempt = 0
  while (true) {
    try {
      return await downloadOnce(client, target, onProgress)
    } catch (error) {
      if (
        error instanceof Error &&
        'retryable' in error &&
        (error as { retryable?: boolean }).retryable &&
        attempt < RETRY_DELAYS_MS.length
      ) {
        await delay(RETRY_DELAYS_MS[attempt]!)
        attempt += 1
        continue
      }
      throw error
    }
  }
}

async function downloadOnce(
  client: PolarisClient,
  target: DownloadTarget,
  onProgress?: (event: SyncProgressEvent) => void,
): Promise<number> {
  const { snapshot } = target
  await fs.mkdir(path.dirname(snapshot.localPath), { recursive: true })
  await fs.mkdir(path.dirname(snapshot.tempPath), { recursive: true })

  await fs.rm(snapshot.tempPath, { force: true })
  const response = target.directUrl
    ? await client.downloadFromUrl(target.directUrl, snapshot.key)
    : await client.downloadSnapshot(snapshot.key)
  const totalBytesHeader = response.headers.get('content-length')
  const totalBytes = totalBytesHeader ? Number.parseInt(totalBytesHeader, 10) : undefined
  onProgress?.(
    totalBytes === undefined
      ? { type: 'started', key: snapshot.key }
      : { type: 'started', key: snapshot.key, totalBytes },
  )

  if (snapshot.state !== 'present') {
    await fs.rm(snapshot.localPath, { force: true })
  }

  const file = await fs.open(snapshot.tempPath, 'w')
  let downloadedBytes = 0
  try {
    for await (const chunk of response.body ?? []) {
      const buffer = Buffer.from(chunk)
      await file.write(buffer)
      downloadedBytes += buffer.length
      onProgress?.(
        totalBytes === undefined
          ? { type: 'progress', key: snapshot.key, downloadedBytes }
          : { type: 'progress', key: snapshot.key, downloadedBytes, totalBytes },
      )
    }
    await file.sync()
  } finally {
    await file.close()
  }

  await fs.rm(snapshot.localPath, { force: true })
  await fs.rename(snapshot.tempPath, snapshot.localPath)
  return downloadedBytes
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

async function resolveDownloadTargets(
  client: PolarisClient,
  plan: SyncPlan,
): Promise<DownloadTarget[]> {
  const snapshots = missingSnapshots(plan)
  const byDate = new Map<string, SnapshotPlan[]>()
  for (const snapshot of snapshots) {
    const date = inferDateFromText(snapshot.key)
    if (!date) continue
    const group = byDate.get(date)
    if (group) group.push(snapshot)
    else byDate.set(date, [snapshot])
  }

  const directUrls = new Map<string, string>()
  await Promise.all(
    [...byDate.entries()].map(async ([date, daySnapshots]) => {
      try {
        const manifest = await client.downloadBatchManifest(plan.source, plan.market, date)
        for (const snapshot of manifest.snapshots) {
          directUrls.set(snapshot.key, snapshot.url)
        }
      } catch {
        // Fall back to individual key resolution for this date.
        for (const snapshot of daySnapshots) directUrls.delete(snapshot.key)
      }
    }),
  )

  return snapshots.map((snapshot) => {
    const directUrl = directUrls.get(snapshot.key)
    return directUrl === undefined ? { snapshot } : { snapshot, directUrl }
  })
}
