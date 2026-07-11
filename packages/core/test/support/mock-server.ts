import http from 'node:http'
import { once } from 'node:events'

export interface SnapshotFixture {
  source: string
  market: string
  coverage: { start: string; end: string }
  pages: Array<Array<{ key: string; date?: string }>>
  totalBytes: number
  files: Record<string, Uint8Array>
  failuresRemaining?: Record<string, number>
  marketAvailable?: boolean
}

export class MockPolarisServer {
  readonly server: http.Server
  readonly state: {
    feedbackMessages: string[]
    failuresRemaining: Map<string, number>
    batchDownloadCount: number
    keyDownloadCount: number
  }

  constructor(readonly fixture: SnapshotFixture) {
    this.state = {
      feedbackMessages: [],
      failuresRemaining: new Map(Object.entries(fixture.failuresRemaining ?? {})),
      batchDownloadCount: 0,
      keyDownloadCount: 0,
    }
    this.server = http.createServer(this.handle.bind(this))
  }

  async start(): Promise<void> {
    this.server.listen(0, '127.0.0.1')
    await once(this.server, 'listening')
  }

  async close(): Promise<void> {
    this.server.close()
    await once(this.server, 'close')
  }

  baseUrl(): string {
    const address = this.server.address()
    if (!address || typeof address === 'string') throw new Error('server not listening')
    return `http://127.0.0.1:${address.port}`
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse) {
    const url = new URL(req.url ?? '/', this.baseUrl())
    const method = req.method ?? 'GET'

    if (method === 'GET' && url.pathname === '/catalog') {
      return this.json(res, {
        markets:
          this.fixture.marketAvailable === false
            ? []
            : [
                {
                  source: this.fixture.source,
                  market: this.fixture.market,
                  start: this.fixture.coverage.start,
                  end: this.fixture.coverage.end,
                  source_type: 'manifest',
                  access: { status: 'preview', public_cutoff_date: '2026-05-28' },
                  categories: ['Futures'],
                },
              ],
      })
    }

    if (method === 'GET' && url.pathname === '/snapshots') {
      const cursor = Number.parseInt(url.searchParams.get('cursor') ?? '0', 10) || 0
      const page = this.fixture.pages[cursor] ?? []
      return this.json(res, {
        total: this.fixture.pages.flat().length,
        total_bytes: this.fixture.totalBytes,
        next_cursor: cursor + 1 < this.fixture.pages.length ? String(cursor + 1) : null,
        snapshots: page,
      })
    }

    if (method === 'GET' && url.pathname === '/download') {
      const source = url.searchParams.get('source')
      const market = url.searchParams.get('market')
      const date = url.searchParams.get('date')
      const mode = url.searchParams.get('mode')
      if (source && market && date && mode === 'json') {
        this.state.batchDownloadCount += 1
        const snapshots = this.fixture.pages
          .flat()
          .filter((snapshot) => snapshot.key.includes(date))
          .map((snapshot) => ({
            date,
            timestamp: extractTimestamp(snapshot.key, date),
            key: snapshot.key,
            url: `${this.baseUrl()}/files/${encodeURIComponent(snapshot.key)}`,
            expires_in_seconds: 86400,
          }))
        return this.json(res, {
          source,
          market,
          date,
          total: snapshots.length,
          total_bytes: snapshots.reduce(
            (sum, snapshot) => sum + (this.fixture.files[snapshot.key]?.byteLength ?? 0),
            0,
          ),
          snapshots,
        })
      }

      const key = url.searchParams.get('key')
      if (!key || !this.fixture.files[key]) {
        return this.error(res, 404, 'missing key')
      }
      this.state.keyDownloadCount += 1
      return this.json(res, {
        url: `${this.baseUrl()}/files/${encodeURIComponent(key)}`,
      })
    }

    if (method === 'GET' && url.pathname.startsWith('/files/')) {
      const key = decodeURIComponent(url.pathname.replace('/files/', ''))
      const remaining = this.state.failuresRemaining.get(key) ?? 0
      if (remaining > 0) {
        this.state.failuresRemaining.set(key, remaining - 1)
        return this.error(res, 500, 'retry me')
      }
      const bytes = this.fixture.files[key]
      if (!bytes) return this.error(res, 404, 'missing file')
      res.writeHead(200, { 'content-length': bytes.byteLength, 'content-type': 'application/octet-stream' })
      res.end(bytes)
      return
    }

    if (method === 'GET' && url.pathname === '/account') {
      return this.json(res, {
        user_id: 'user_123',
        auth: { provider: 'api_key', key_id: 'key-live' },
        identity: {
          display_name: 'Test User',
          email: 'test@example.com',
        },
        subscription: { tier: 'pro' },
      })
    }

    if (method === 'POST' && url.pathname === '/feedback') {
      const body = await readBody(req)
      this.state.feedbackMessages.push(JSON.parse(body).message)
      return this.json(res, { ok: true })
    }

    if (method === 'POST' && url.pathname === '/auth/cli/start') {
      return this.json(res, {
        request_id: 'req_123',
        poll_token: 'poll_123',
        user_code: 'ABCD-1234',
        login_url: `${this.baseUrl()}/login`,
        expires_at: '2026-07-11T12:00:00Z',
        interval_ms: 10,
      })
    }

    if (method === 'GET' && url.pathname === '/auth/cli/poll') {
      return this.json(res, {
        status: 'approved',
        request_id: 'req_123',
        user_id: 'user_123',
        display_name: 'Test User',
        email: 'test@example.com',
        api_key: 'api-key',
      })
    }

    this.error(res, 404, 'not found')
  }

  private json(res: http.ServerResponse, body: unknown) {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(body))
  }

  private error(res: http.ServerResponse, status: number, message: string) {
    res.writeHead(status, { 'content-type': 'text/plain' })
    res.end(message)
  }
}

export function basicFixture(): SnapshotFixture {
  const pages = [
    [{ key: 'standard-aster-BTCUSDT-2026-06-01-00', date: '2026-06-01' }],
    [{ key: 'standard-aster-BTCUSDT-2026-06-01-01', date: '2026-06-01' }],
  ]
  const files = {
    'standard-aster-BTCUSDT-2026-06-01-00': new TextEncoder().encode('snapshot-0'),
    'standard-aster-BTCUSDT-2026-06-01-01': new TextEncoder().encode('snapshot-1'),
  }
  return {
    source: 'aster',
    market: 'BTCUSDT',
    coverage: {
      start: '2026-06-01T00:00:00Z',
      end: '2026-06-02T00:00:00Z',
    },
    pages,
    totalBytes: Object.values(files).reduce((sum, value) => sum + value.byteLength, 0),
    files,
  }
}

async function readBody(req: http.IncomingMessage): Promise<string> {
  const chunks: Buffer[] = []
  for await (const chunk of req) chunks.push(Buffer.from(chunk))
  return Buffer.concat(chunks).toString('utf8')
}

function extractTimestamp(key: string, date: string): string {
  const marker = `${date}-`
  const index = key.indexOf(marker)
  if (index === -1) return '000000'
  return key.slice(index + marker.length) || '000000'
}
