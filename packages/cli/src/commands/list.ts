import { Layout } from '@polaris/core';
import type { Config, LocalSnapshotEntry } from '@polaris/core';

import type { LocalListOutput, LocalSnapshot } from '../schemas.js';
import { matchesExact } from '../render/helpers.js';

export async function runListCommand(
  config: Config,
  filters: { source: string | null; market: string | null; date: string | null },
): Promise<LocalListOutput> {
  const entries = await new Layout(config.root).listLocalSnapshots();
  const snapshots = entries
    .filter((entry) => matchesExact(entry.source ?? null, filters.source))
    .filter((entry) => matchesExact(entry.market ?? null, filters.market))
    .filter((entry) => matchesExact(entry.date ?? null, filters.date))
    .map((entry) => toLocalSnapshotJson(entry));

  return {
    command: 'list',
    root: config.root,
    filters,
    snapshot_total: snapshots.length,
    snapshots,
  };
}

function toLocalSnapshotJson(entry: LocalSnapshotEntry): LocalSnapshot {
  return {
    key: entry.key,
    path: entry.path,
    filename: entry.filename,
    source: entry.source ?? null,
    market: entry.market ?? null,
    date: entry.date ?? null,
  };
}
