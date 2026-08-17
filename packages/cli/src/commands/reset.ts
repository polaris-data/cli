import fs from 'node:fs/promises'

import { Layout, acquireSyncLock, clearBookmarks } from '@polaris/core'
import type { Config } from '@polaris/core'

import type { ResetOutput } from '../schemas.js'

export async function runResetCommand(config: Config): Promise<ResetOutput> {
  const layout = new Layout(config.root)
  const guard = await acquireSyncLock(layout)
  try {
    const snapshotTotal = (await layout.listLocalSnapshots()).length
    const candidateRoots = [layout.dataRoot(), layout.tmpRoot(), layout.cacheRoot()]
    const removedRoots: string[] = []
    for (const root of candidateRoots) {
      try {
        await fs.rm(root, { recursive: true })
        removedRoots.push(root)
      } catch (error) {
        const err = error as NodeJS.ErrnoException
        if (err.code !== 'ENOENT') throw error
      }
    }
    await clearBookmarks(config.root)
    return {
      command: 'reset',
      root: config.root,
      snapshot_total: snapshotTotal,
      removed_roots: removedRoots,
    }
  } finally {
    await guard.release()
  }
}
