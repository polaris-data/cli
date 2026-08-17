import { invalidArgument } from '@polaris/core'
import type { CatalogMarket, Config, PolarisClient } from '@polaris/core'

import type { RemoteDatasetEntry, RemoteListOutput } from '../schemas.js'
import { accessSummary, matchesExact } from '../render/helpers.js'

export async function runCatalogCommand(
  config: Config,
  client: PolarisClient,
  filters: { source: string | null; market: string | null; search: string | null; limit: number },
): Promise<{ output: RemoteListOutput }> {
  if (filters.limit <= 0) throw invalidArgument('--limit must be greater than zero')
  const catalog = await client.fetchCatalog(filters.source ?? undefined, filters.market ?? undefined)
  const datasets = filterRemoteCatalog(catalog.markets, filters, filters.limit)
  return {
    output: {
      command: 'catalog',
      filters,
      dataset_total: datasets.length,
      datasets,
    },
  }
}

function filterRemoteCatalog(
  markets: CatalogMarket[],
  filters: { source: string | null; market: string | null; search: string | null },
  limit: number,
): RemoteDatasetEntry[] {
  const datasets = markets
    .filter((market) => matchesExact(market.source, filters.source))
    .filter((market) => matchesExact(market.market, filters.market))
    .map((market) => toRemoteDatasetEntry(market))
    .filter((entry) => matchesSearch(entry, filters.search))
    .sort(
      (left, right) =>
        accessSortOrder(left.access) - accessSortOrder(right.access) ||
        left.dataset.localeCompare(right.dataset),
    )

  return datasets.slice(0, limit)
}

function toRemoteDatasetEntry(market: CatalogMarket): RemoteDatasetEntry {
  const entry: RemoteDatasetEntry = {
    source: market.source,
    market: market.market,
    start: market.start,
    end: market.end,
    catalog_source: market.catalog_source ?? null,
    access: market.access
      ? {
          status: market.access.status,
          public_cutoff_date: market.access.public_cutoff_date ?? null,
        }
      : null,
    dataset: `${market.source}:${market.market}`,
  }
  if (market.categories.length > 0) entry.categories = market.categories
  return entry
}

function matchesSearch(entry: RemoteDatasetEntry, search: string | null): boolean {
  const normalized = search?.trim().toLowerCase()
  if (!normalized) return true
  const haystack = [
    entry.dataset,
    entry.catalog_source ?? '',
    ...(entry.categories ?? []),
    accessSummary(entry.access),
  ]
    .join(' ')
    .toLowerCase()
  return normalized.split(/\s+/).every((token) => haystack.includes(token))
}

function accessSortOrder(access: RemoteDatasetEntry['access']): number {
  if (!access) return Number.MAX_SAFE_INTEGER
  switch (access.status) {
    case 'open':
      return 0
    case 'preview':
      return 1
    case 'restricted':
      return 2
  }
}
