import { invalidArgument, otherError, requestError } from './errors.js'
import { toRfc3339 } from './time.js'
import type {
  AccountResponse,
  CatalogMarket,
  CatalogResponse,
  CliAuthPollResponse,
  CliAuthStartResponse,
  FeedbackResponse,
  SnapshotEntry,
} from './types.js'

interface StandardSnapshotsPageWire {
  total?: number
  total_bytes?: number
  next_cursor?: string | null
  data?: SnapshotEntryWire[]
  snapshots?: SnapshotEntryWire[]
}

interface SnapshotEntryWire {
  key?: string
  path?: string
  name?: string
  date?: string
}

interface DownloadResponse {
  url: string
}

interface BatchDownloadManifestWire {
  source: string
  market: string
  date: string
  total: number
  total_bytes?: number
  snapshots: BatchDownloadSnapshotWire[]
}

interface BatchDownloadSnapshotWire {
  date: string
  timestamp: string
  key: string
  url: string
  expires_in_seconds?: number
}

interface CatalogResponseWire {
  markets?: CatalogMarketWire[]
  sources?: LegacyCatalogSourceWire[]
  updatedAt?: string
}

interface CatalogMarketWire {
  source: string
  market: string
  start: string
  end: string
  source_type?: string
  category?: string
  categories?: string[]
  access?: {
    status: 'open' | 'preview' | 'restricted'
    public_cutoff_date?: string
  }
}

interface LegacyCatalogSourceWire {
  id: string
  markets?: LegacyCatalogMarketWire[]
}

interface LegacyCatalogMarketWire {
  id: string
  start: string
  end: string
  source?: string
  category?: string
  categories?: string[]
  access?: {
    status: 'open' | 'preview' | 'restricted'
    public_cutoff_date?: string
  }
}

export class PolarisClient {
  readonly baseUrl: string

  constructor(
    baseUrl: string,
    readonly apiKey: string | undefined,
    readonly timeoutMs: number,
    readonly apiFetch: typeof fetch = fetch,
    readonly downloadFetch: typeof fetch = fetch,
  ) {
    this.baseUrl = baseUrl.trim().replace(/\/+$/, '')
  }

  hasApiKey(): boolean {
    return Boolean(this.apiKey)
  }

  async startCliAuth(): Promise<CliAuthStartResponse> {
    return this.decodeJsonResponse<CliAuthStartResponse>(
      await this.apiFetch(`${this.baseUrl}/auth/cli/start`, {
        method: 'POST',
        redirect: 'manual',
        signal: AbortSignal.timeout(this.timeoutMs),
      }).catch((error) => {
        throw otherError('CLI auth start request failed', error)
      }),
      'CLI auth start request failed',
    )
  }

  async fetchAccount(): Promise<AccountResponse> {
    return this.sendJson<AccountResponse>(`${this.baseUrl}/account`, {}, 'account request failed')
  }

  async submitFeedback(message: string): Promise<FeedbackResponse> {
    return this.sendJson<FeedbackResponse>(
      `${this.baseUrl}/feedback`,
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ message }),
      },
      'feedback request failed',
    )
  }

  async pollCliAuth(requestId: string, pollToken: string): Promise<CliAuthPollResponse> {
    const url = new URL(`${this.baseUrl}/auth/cli/poll`)
    url.searchParams.set('request_id', requestId)
    url.searchParams.set('poll_token', pollToken)
    const response = await this.apiFetch(url, {
      redirect: 'manual',
      signal: AbortSignal.timeout(this.timeoutMs),
    }).catch((error) => {
      throw otherError('CLI auth poll request failed', error)
    })
    const body = await response.text()
    if (!(response.ok || response.status === 410)) {
      throw httpError(response.status, body, 'CLI auth poll request failed')
    }
    try {
      return JSON.parse(body) as CliAuthPollResponse
    } catch (error) {
      throw otherError(
        `CLI auth poll request failed: failed to decode JSON response: ${bodySnippet(body)}`,
        error,
      )
    }
  }

  async fetchCatalog(source?: string, market?: string): Promise<CatalogResponse> {
    if (market && !source) {
      throw invalidArgument('--market on remote list requires --source')
    }

    const url = new URL(`${this.baseUrl}/catalog`)
    if (source) url.searchParams.set('source', source)
    if (market) url.searchParams.set('market', market)

    const raw = await this.sendJson<CatalogResponseWire>(url, {}, 'catalog request failed')
    return normalizeCatalog(raw)
  }

  async listSnapshots(
    source: string,
    market: string,
    from: Date,
    to: Date,
  ): Promise<{ snapshots: SnapshotEntry[]; totalRemoteBytes: number }> {
    let cursor: string | undefined
    const all: SnapshotEntry[] = []
    let totalRemoteBytes = 0

    while (true) {
      const url = new URL(`${this.baseUrl}/snapshots`)
      url.searchParams.set('source', source)
      url.searchParams.set('market', market)
      url.searchParams.set('from', toRfc3339(from))
      url.searchParams.set('to', toRfc3339(to))
      url.searchParams.set('limit', '1000')
      if (cursor) url.searchParams.set('cursor', cursor)

      const page = await this.sendJson<StandardSnapshotsPageWire>(
        url,
        {},
        'snapshot listing failed',
      )
      const snapshotEntries = [...(page.snapshots ?? []), ...(page.data ?? [])]
      const normalized = snapshotEntries.map(intoSnapshot)
      if (all.length === 0) totalRemoteBytes = page.total_bytes ?? 0
      all.push(...normalized)

      cursor = page.next_cursor ?? undefined
      if (!cursor) {
        if (page.total && page.total !== all.length && page.total !== 0) {
          throw otherError(
            `snapshot pagination returned ${all.length} entries but advertised ${page.total}`,
          )
        }
        break
      }
    }

    return { snapshots: all, totalRemoteBytes }
  }

  async downloadBatchManifest(
    source: string,
    market: string,
    date: string,
  ): Promise<{
    source: string
    market: string
    date: string
    total: number
    totalBytes: number
    snapshots: Array<{
      date: string
      timestamp: string
      key: string
      url: string
      expiresInSeconds?: number
    }>
  }> {
    const url = new URL(`${this.baseUrl}/download`)
    url.searchParams.set('source', source)
    url.searchParams.set('market', market)
    url.searchParams.set('date', date)
    url.searchParams.set('mode', 'json')

    const manifest = await this.sendJson<BatchDownloadManifestWire>(
      url,
      {},
      `download manifest request failed for ${source}/${market}/${date}`,
    )

    return {
      source: manifest.source,
      market: manifest.market,
      date: manifest.date,
      total: manifest.total,
      totalBytes: manifest.total_bytes ?? 0,
      snapshots: manifest.snapshots.map((snapshot) =>
        snapshot.expires_in_seconds === undefined
          ? {
              date: snapshot.date,
              timestamp: snapshot.timestamp,
              key: snapshot.key,
              url: snapshot.url,
            }
          : {
              date: snapshot.date,
              timestamp: snapshot.timestamp,
              key: snapshot.key,
              url: snapshot.url,
              expiresInSeconds: snapshot.expires_in_seconds,
            },
      ),
    }
  }

  async downloadSnapshot(key: string): Promise<Response> {
    const url = new URL(`${this.baseUrl}/download`)
    url.searchParams.set('key', key)
    url.searchParams.set('mode', 'json')
    const response = await this.authorizedFetch(url, {}, this.apiFetch, `download request failed for ${key}`)
    const body = await response.text()
    if (!response.ok) throw httpError(response.status, body, 'download request failed')

    let download: DownloadResponse
    try {
      download = JSON.parse(body) as DownloadResponse
    } catch (error) {
      throw otherError(`failed to parse download response: ${bodySnippet(body)}`, error)
    }

    return this.downloadFromUrl(download.url, key)
  }

  async downloadFromUrl(url: string, key: string): Promise<Response> {
    const fileResponse = await this.downloadFetch(url, {
      signal: AbortSignal.timeout(this.timeoutMs),
    }).catch((error) => {
      throw otherError(`file download failed for ${key}`, error)
    })
    if (fileResponse.ok) return fileResponse
    throw httpError(fileResponse.status, await fileResponse.text(), 'file download failed')
  }

  private async sendJson<T>(
    input: string | URL,
    init: RequestInit,
    context: string,
  ): Promise<T> {
    return this.decodeJsonResponse<T>(
      await this.authorizedFetch(input, init, this.apiFetch, context),
      context,
    )
  }

  private async decodeJsonResponse<T>(response: Response, context: string): Promise<T> {
    if (!response.ok) {
      throw httpError(response.status, await response.text(), context)
    }
    const body = await response.text()
    try {
      return JSON.parse(body) as T
    } catch (error) {
      throw otherError(`${context}: failed to decode JSON response: ${bodySnippet(body)}`, error)
    }
  }

  private async authorizedFetch(
    input: string | URL,
    init: RequestInit,
    client: typeof fetch,
    context: string,
  ): Promise<Response> {
    const headers = new Headers(init.headers)
    if (this.apiKey) headers.set('authorization', `Bearer ${this.apiKey}`)
    try {
      return await client(input, {
        ...init,
        headers,
        redirect: init.redirect ?? 'manual',
        signal: init.signal ?? AbortSignal.timeout(this.timeoutMs),
      })
    } catch (error) {
      throw otherError(context, error)
    }
  }
}

export function bodySnippet(body: string): string {
  const trimmed = body.trim()
  if (!trimmed) return '<empty body>'
  return trimmed.length > 240 ? `${trimmed.slice(0, 240)}...` : trimmed
}

export function httpError(status: number, body: string, context: string) {
  const message = body.trim() ? `${context} (${status}): ${body.trim()}` : `${context} (${status})`
  return requestError(status, message, status === 429 || status >= 500)
}

function intoSnapshot(wire: SnapshotEntryWire): SnapshotEntry {
  const key = wire.key ?? wire.path ?? wire.name
  if (!key) throw otherError('snapshot listing failed: missing snapshot key')
  return wire.date ? { key, date: wire.date } : { key }
}

function normalizeCatalog(raw: CatalogResponseWire): CatalogResponse {
  const directMarkets = raw.markets ?? []
  const markets =
    directMarkets.length > 0
      ? directMarkets.map(normalizeCatalogMarket)
      : (raw.sources ?? []).flatMap((source) =>
          (source.markets ?? []).map((market) =>
            compactOptional({
              source: source.id,
              market: market.id,
              start: market.start,
              end: market.end,
              catalog_source: market.source,
              categories: normalizeCategories(market.category, market.categories),
              access: market.access,
            }),
          ),
        )
  return { markets, updated_at: raw.updatedAt }
}

function normalizeCatalogMarket(wire: CatalogMarketWire): CatalogMarket {
  return compactOptional({
    source: wire.source,
    market: wire.market,
    start: wire.start,
    end: wire.end,
    catalog_source: wire.source_type,
    categories: normalizeCategories(wire.category, wire.categories),
    access: wire.access,
  })
}

function normalizeCategories(category?: string, categories?: string[]): string[] {
  if (categories?.length) return categories
  if (category) return [category]
  return []
}

function compactOptional<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  ) as T
}
