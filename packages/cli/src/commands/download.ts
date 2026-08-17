import {
  Layout,
  acquireSyncLock,
  buildSyncPlan,
  executeSync,
  invalidArgument,
  parseRfc3339,
  presentTotal,
} from '@polaris/core'
import type { Config, PolarisClient, SyncExecution, SyncPlan } from '@polaris/core'

import type { SyncOutput } from '../schemas.js'

export async function runDownloadCommand(
  config: Config,
  client: PolarisClient,
  options: {
    source: string
    market: string
    from: string
    to: string
    concurrency?: number | undefined
  },
): Promise<{ output: SyncOutput; exitCode: number }> {
  const requestedRange = {
    from: parseRfc3339(options.from, '--from').toISOString(),
    to: parseRfc3339(options.to, '--to').toISOString(),
  }
  if (requestedRange.from > requestedRange.to) {
    throw invalidArgument('--from must be less than or equal to --to')
  }

  const layout = new Layout(config.root)
  const guard = await acquireSyncLock(layout)
  try {
    const plan = await buildSyncPlan(client, config, options.source, options.market, requestedRange)
    const concurrency = options.concurrency ?? config.concurrency
    if (concurrency <= 0) throw invalidArgument('--concurrency must be greater than zero')
    const execution = await executeSync(client, plan, concurrency)
    const output = toSyncOutput(plan, execution)
    return { output, exitCode: output.failed_total > 0 ? 1 : 0 }
  } finally {
    await guard.release()
  }
}

function toSyncOutput(plan: SyncPlan, execution: SyncExecution): SyncOutput {
  return {
    command: 'download',
    source: plan.source,
    market: plan.market,
    requested_range: plan.requestedRange,
    effective_range: plan.effectiveRange,
    root: plan.root,
    remote_total: plan.snapshots.length,
    downloaded_total: execution.downloadedKeys.length,
    skipped_total: presentTotal(plan),
    failed_total: execution.failed.length,
    downloaded_keys: execution.downloadedKeys,
    failed: execution.failed,
  }
}
