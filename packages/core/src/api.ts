import {
  PolarisClient as SdkPolarisClient,
  PolarisError as SdkPolarisError,
} from 'polaris-data'

import { invalidArgument, otherError, requestError } from './errors.js'
import type {
  AccountResponse,
  CatalogResponse,
  CliAuthPollResponse,
  CliAuthStartResponse,
  DatasetAccessStatus,
  FeedbackResponse,
  SnapshotEntry,
} from './types.js'

interface DownloadResponse {
  url: string
}

export class PolarisClient {
  readonly baseUrl: string
  private readonly sdk: SdkPolarisClient

  constructor(
    baseUrl: string,
    readonly apiKey: string | undefined,
    readonly timeoutMs: number,
    readonly apiFetch: typeof fetch = fetch,
    readonly downloadFetch: typeof fetch = fetch,
  ) {
    this.baseUrl = baseUrl.trim().replace(/\/+$/, '')
    const options: ConstructorParameters<typeof SdkPolarisClient>[0] = {
      baseUrl: this.baseUrl,
      timeout: timeoutMs,
      fetch: apiFetch,
    }
    if (apiKey) options.apiKey = apiKey
    this.sdk = new SdkPolarisClient(options)
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

    try {
      const catalogOptions: { source?: string; market?: string } = {}
      if (source) catalogOptions.source = source
      if (market) catalogOptions.market = market
      const catalog = await this.sdk.catalog(catalogOptions)
      return {
        markets: catalog.markets.map((entry) => ({
          source: entry.source,
          market: entry.market,
          start: entry.start ?? '',
          end: entry.end ?? '',
          catalog_source: entry.source_type,
          categories: entry.categories ?? [],
          access: entry.access
            ? {
                status: entry.access.status as DatasetAccessStatus,
                public_cutoff_date: entry.access.public_cutoff_date ?? undefined,
              }
            : undefined,
        })),
        updated_at: catalog.updatedAt,
      }
    } catch (error) {
      throw mapSdkError(error, 'catalog request failed')
    }
  }

  async listSnapshots(
    source: string,
    market: string,
    from: Date,
    to: Date,
  ): Promise<{ snapshots: SnapshotEntry[]; totalRemoteBytes: number }> {
    try {
      const entries = await this.sdk.listSnapshots({
        source,
        market,
        from,
        to,
      })
      return {
        snapshots: entries.map((entry) => ({ key: entry.key, date: entry.date })),
        totalRemoteBytes: 0,
      }
    } catch (error) {
      throw mapSdkError(error, 'snapshot listing failed')
    }
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
    try {
      const manifest = await this.sdk.getSnapshotDownloadUrls({ source, market, date })
      return {
        source: manifest.source,
        market: manifest.market,
        date: manifest.date,
        total: manifest.total,
        totalBytes: manifest.total_bytes ?? 0,
        snapshots: manifest.snapshots.map((snapshot) => ({
          date: snapshot.date,
          timestamp: snapshot.timestamp,
          key: snapshot.key,
          url: snapshot.url,
          expiresInSeconds: snapshot.expires_in_seconds,
        })),
      }
    } catch (error) {
      throw mapSdkError(
        error,
        `download manifest request failed for ${source}/${market}/${date}`,
      )
    }
  }

  async downloadSnapshot(key: string): Promise<Response> {
    let download: DownloadResponse
    try {
      download = await this.sdk.getSnapshotDownloadUrl({ key, mode: 'json' })
    } catch (error) {
      throw mapSdkError(error, 'download request failed')
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

function mapSdkError(error: unknown, context: string) {
  if (error instanceof SdkPolarisError) {
    const status = error.statusCode
    const retryable = status === 429 || (status !== undefined && status >= 500)
    return requestError(status, `${context}: ${error.message}`, retryable)
  }
  if (error instanceof Error) return otherError(context, error)
  return otherError(context, new Error(String(error)))
}
