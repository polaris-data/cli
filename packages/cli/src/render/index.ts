import type { LocalListOutput, RemoteListOutput, ResetOutput, SyncOutput } from '../schemas.js';
import { accessSummary } from './helpers.js';

export function renderRemoteListOutput(output: RemoteListOutput): string {
  const lines = ['catalog'];
  if (output.filters.source || output.filters.market || output.filters.search) {
    lines.push(
      `filters: source=${formatMaybe(output.filters.source)} market=${formatMaybe(output.filters.market)} search=${formatMaybe(output.filters.search)}`,
    );
  }
  lines.push(`datasets: ${output.dataset_total}`);
  if (output.datasets.length > 0) {
    lines.push('remote datasets:');
    for (const dataset of output.datasets) {
      lines.push(
        `  ${dataset.source}:${dataset.market} ${dataset.start} -> ${dataset.end} (${accessSummary(dataset.access)})`,
      );
    }
  }
  return lines.join('\n');
}

export function renderLocalListOutput(output: LocalListOutput): string {
  const lines = ['list', `root: ${output.root}`];
  if (output.filters.source || output.filters.market || output.filters.date) {
    lines.push(
      `filters: source=${formatMaybe(output.filters.source)} market=${formatMaybe(output.filters.market)} date=${formatMaybe(output.filters.date)}`,
    );
  }
  lines.push(`snapshots: ${output.snapshot_total}`);
  if (output.snapshots.length > 0) {
    lines.push('local snapshots:');
    for (const snapshot of output.snapshots.slice(0, 50)) lines.push(`  ${snapshot.key}`);
    if (output.snapshots.length > 50) {
      lines.push(`  ... ${output.snapshots.length - 50} more`);
    }
  }
  return lines.join('\n');
}

export function renderSyncOutput(output: SyncOutput): string {
  const lines = [
    `download ${output.source} ${output.market}`,
    `root: ${output.root}`,
    `requested: ${output.requested_range.from} -> ${output.requested_range.to}`,
    `effective: ${output.effective_range.from} -> ${output.effective_range.to}`,
    `remote: ${output.remote_total}`,
    `downloaded: ${output.downloaded_total}`,
    `skipped: ${output.skipped_total}`,
    `failed: ${output.failed_total}`,
  ];
  if (output.failed.length > 0) {
    lines.push('failed keys:');
    for (const failure of output.failed) lines.push(`  ${failure.key}: ${failure.error}`);
  }
  return lines.join('\n');
}

export function renderResetOutput(output: ResetOutput): string {
  const lines = ['reset', `root: ${output.root}`, `removed snapshots: ${output.snapshot_total}`];
  if (output.removed_roots.length > 0) {
    lines.push('removed roots:');
    for (const root of output.removed_roots) lines.push(`  ${root}`);
  }
  return lines.join('\n');
}

export function formatCommandResult<T>(
  formatExplicit: boolean,
  jsonValue: T,
  human: string,
  meta?: { cta?: { commands: Array<Record<string, unknown>> } | undefined },
): T | string {
  if (formatExplicit) {
    return jsonValue;
  }
  if (meta?.cta) {
    const next = meta.cta.commands
      .map((command) => {
        const name = String(command.command);
        const description = command.description ? ` - ${String(command.description)}` : '';
        return `  ${name}${description}`;
      })
      .join('\n');
    return `${human}\nNext:\n${next}`;
  }
  return human;
}

function formatMaybe(value: string | null): string {
  return value === null ? 'None' : JSON.stringify(value);
}
