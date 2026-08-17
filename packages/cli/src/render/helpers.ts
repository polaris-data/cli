import type { RemoteDatasetEntry } from '../schemas.js'

export function matchesExact(value: string | null, filter: string | null): boolean {
  return filter === null ? true : value === filter
}

export function accessSummary(access: RemoteDatasetEntry['access']): string {
  if (!access) return 'unknown'
  if (access.status === 'preview' && access.public_cutoff_date) {
    return `preview from ${access.public_cutoff_date}`
  }
  return access.status
}
