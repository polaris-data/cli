import fs from 'node:fs/promises'

import { datasetUnavailable } from './errors.js'
import { Layout } from './layout.js'
import type { PolarisClient } from './api.js'
import type { Config, SnapshotPlan, SyncPlan, TimeWindow } from './types.js'

export function intersectRanges(
  requested: TimeWindow,
  available: TimeWindow,
): TimeWindow | undefined {
  const from = requested.from > available.from ? requested.from : available.from
  const to = requested.to < available.to ? requested.to : available.to
  return from > to ? undefined : { from, to }
}

export async function buildSyncPlan(
  client: PolarisClient,
  config: Config,
  source: string,
  market: string,
  requestedRange: TimeWindow,
): Promise<SyncPlan> {
  const layout = new Layout(config.root)
  const catalog = await client.fetchCatalog(source, market)
  const coverage = catalog.markets.find((entry) => entry.source === source && entry.market === market)
  if (!coverage) {
    throw datasetUnavailable(`dataset source/market ${source}/${market} is not available`)
  }

  const effectiveRange = intersectRanges(requestedRange, {
    from: coverage.start,
    to: coverage.end,
  })
  if (!effectiveRange) {
    throw datasetUnavailable(
      `requested range does not overlap remote coverage for source/market ${source}/${market}`,
    )
  }

  const remote = await client.listSnapshots(
    source,
    market,
    new Date(effectiveRange.from),
    new Date(effectiveRange.to),
  )
  const snapshots = await classifySnapshots(layout, remote.snapshots)

  return {
    source,
    market,
    requestedRange,
    effectiveRange,
    root: config.root,
    totalRemoteBytes: remote.totalRemoteBytes,
    snapshots,
  }
}

export async function classifySnapshots(
  layout: Layout,
  remoteSnapshots: Array<{ key: string }>,
): Promise<SnapshotPlan[]> {
  const snapshots: SnapshotPlan[] = []
  for (const snapshot of remoteSnapshots) {
    const localPath = layout.dataPathForKey(snapshot.key)
    const tempPath = layout.tempPathForKey(snapshot.key)

    const tempExists = await pathExists(tempPath)
    const metadata = await statOrUndefined(localPath)
    let state: SnapshotPlan['state']
    let localSize = 0
    if (tempExists) {
      state = 'incomplete'
    } else if (metadata && metadata.size > 0) {
      state = 'present'
      localSize = metadata.size
    } else {
      state = 'missing'
    }

    snapshots.push({
      key: snapshot.key,
      localPath,
      tempPath,
      localSize,
      state,
    })
  }

  return snapshots
}

export function remoteTotal(plan: SyncPlan): number {
  return plan.snapshots.length
}

export function presentTotal(plan: SyncPlan): number {
  return plan.snapshots.filter((snapshot) => snapshot.state === 'present').length
}

export function missingSnapshots(plan: SyncPlan): SnapshotPlan[] {
  return plan.snapshots.filter((snapshot) => snapshot.state !== 'present')
}

async function pathExists(target: string): Promise<boolean> {
  try {
    await fs.access(target)
    return true
  } catch {
    return false
  }
}

async function statOrUndefined(target: string) {
  try {
    return await fs.stat(target)
  } catch {
    return undefined
  }
}
