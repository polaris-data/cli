export type ApiKeySource = 'environment' | 'credential_store'

export interface Config {
  baseUrl: string
  apiKey?: string | undefined
  apiKeySource?: ApiKeySource | undefined
  root: string
  concurrency: number
  timeoutMs: number
}

export interface CliAuthStartResponse {
  request_id: string
  poll_token: string
  user_code: string
  login_url: string
  expires_at: string
  interval_ms: number
}

export type CliAuthPollResponse =
  | {
      status: 'pending'
      request_id: string
      expires_at: string
      interval_ms: number
    }
  | {
      status: 'approved'
      request_id: string
      user_id: string
      display_name?: string | undefined
      email?: string | undefined
      wallet_address?: string | undefined
      avatar_url?: string | undefined
      api_key: string
    }
  | { status: 'consumed' }
  | { status: 'expired' }

export interface AccountAuth {
  provider: string
  key_id?: string | undefined
}

export interface AccountIdentity {
  display_name?: string | undefined
  email?: string | undefined
  wallet_address?: string | undefined
  avatar_url?: string | undefined
  created_at?: string | undefined
  updated_at?: string | undefined
}

export interface AccountSubscription {
  tier: string
}

export interface AccountResponse {
  user_id: string
  auth: AccountAuth
  identity: AccountIdentity
  subscription: AccountSubscription
}

export interface FeedbackResponse {
  ok: boolean
}

export type DatasetAccessStatus = 'open' | 'preview' | 'restricted'

export interface DatasetAccess {
  status: DatasetAccessStatus
  public_cutoff_date?: string | undefined
}

export interface CatalogMarket {
  source: string
  market: string
  start: string
  end: string
  catalog_source?: string | undefined
  categories: string[]
  access?: DatasetAccess | undefined
}

export interface CatalogResponse {
  markets: CatalogMarket[]
  updated_at?: string | undefined
}

export interface SnapshotEntry {
  key: string
  date?: string | undefined
}

export interface LocalSnapshotEntry {
  key: string
  path: string
  filename: string
  source?: string | undefined
  market?: string | undefined
  date?: string | undefined
  start?: string | undefined
  end?: string | undefined
}

export interface TimeWindow {
  from: string
  to: string
}

export type LocalSnapshotState = 'present' | 'missing' | 'incomplete'

export interface SnapshotPlan {
  key: string
  localPath: string
  tempPath: string
  localSize: number
  state: LocalSnapshotState
}

export interface SyncPlan {
  source: string
  market: string
  requestedRange: TimeWindow
  effectiveRange: TimeWindow
  root: string
  totalRemoteBytes: number
  snapshots: SnapshotPlan[]
}

export interface FailedDownload {
  key: string
  error: string
}

export interface SyncExecution {
  downloadedKeys: string[]
  failed: FailedDownload[]
}

export type SyncProgressEvent =
  | { type: 'started'; key: string; totalBytes?: number | undefined }
  | { type: 'progress'; key: string; downloadedBytes: number; totalBytes?: number | undefined }
  | { type: 'downloaded'; key: string; totalBytes: number }
  | { type: 'failed'; key: string; error: string }

export interface BookmarkStore {
  bookmarks: string[]
}
