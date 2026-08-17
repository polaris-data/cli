import fs from 'node:fs/promises'
import path from 'node:path'

import { Layout } from './layout.js'
import { lockHeld, otherError, requestError } from './errors.js'
import { inferDateFromText } from './layout.js'
import { missingSnapshots } from './planner.js'
import type { PolarisClient } from './api.js'
import type { FailedDownload, SnapshotPlan, SyncExecution, SyncPlan, SyncProgressEvent } from './types.js'
import { delay } from './util.js'

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

export async function acquireSyncLock(layout: Layout): Promise<SyncLockGuard> {
  const lockPath = layout.lockPath()
  await fs.mkdir(path.dirname(lockPath), { recursive: true })
  if (activeLocks.has(lockPath)) throw lockHeld(lockPath)

  activeLocks.add(lockPath)
  try {
    const handle = await openLockFile(lockPath)
    return new SyncLockGuard(lockPath, handle)
  } catch (error) {
    activeLocks.delete(lockPath)
    throw error
  }
}

async function openLockFile(lockPath: string): Promise<fs.FileHandle> {
  try {
    return await createLockFile(lockPath)
  } catch (error) {
    const err = error as NodeJS.ErrnoException
    if (err.code !== 'EEXIST') throw otherError(`failed to open ${lockPath}`, error)
  }

  if (!(await removeStaleLock(lockPath))) throw lockHeld(lockPath)

  try {
    return await createLockFile(lockPath)
  } catch (error) {
    const err = error as NodeJS.ErrnoException
    if (err.code === 'EEXIST') throw lockHeld(lockPath)
    throw otherError(`failed to open ${lockPath}`, error)
  }
}

async function createLockFile(lockPath: string): Promise<fs.FileHandle> {
  const handle = await fs.open(lockPath, 'wx+')
  try {
    await handle.writeFile(String(process.pid), 'utf8')
    return handle
  } catch (error) {
    await handle.close()
    throw error
  }
}

async function removeStaleLock(lockPath: string): Promise<boolean> {
  const pid = await readLockPid(lockPath)
  if (pid !== undefined && isProcessAlive(pid)) return false

  try {
    await fs.rm(lockPath, { force: true })
  } catch {
    // The lock may have been released concurrently; let the caller retry.
  }
  return true
}

async function readLockPid(lockPath: string): Promise<number | undefined> {
  try {
    const pid = Number.parseInt((await fs.readFile(lockPath, 'utf8')).trim(), 10)
    return Number.isFinite(pid) ? pid : undefined
  } catch {
    return undefined
  }
}

function isProcessAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM'
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
  directUrl: string
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
  const response = await client.downloadFromUrl(target.directUrl, snapshot.key)
  const totalBytesHeader = response.headers.get('content-length')
  const totalBytes = totalBytesHeader ? Number.parseInt(totalBytesHeader, 10) : undefined
  onProgress?.(
    totalBytes === undefined
      ? { type: 'started', key: snapshot.key }
      : { type: 'started', key: snapshot.key, totalBytes },
  )

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

async function resolveDownloadTargets(
  client: PolarisClient,
  plan: SyncPlan,
): Promise<DownloadTarget[]> {
  const snapshots = missingSnapshots(plan)
  const dates = new Set<string>()
  for (const snapshot of snapshots) {
    const date = inferDateFromText(snapshot.key)
    if (!date) {
      throw requestError(undefined, `could not resolve download date from snapshot key ${snapshot.key}`)
    }
    dates.add(date)
  }

  const directUrls = new Map<string, string>()
  await Promise.all(
    [...dates].map(async (date) => {
      const manifest = await client.downloadBatchManifest(plan.source, plan.market, date)
      for (const snapshot of manifest.snapshots) {
        directUrls.set(snapshot.key, snapshot.url)
      }
    }),
  )

  return snapshots.map((snapshot) => {
    const directUrl = directUrls.get(snapshot.key)
    if (!directUrl) {
      throw requestError(undefined, `download manifest did not include snapshot ${snapshot.key}`)
    }
    return { snapshot, directUrl }
  })
}
