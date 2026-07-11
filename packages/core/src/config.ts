import fs from 'node:fs'
import path from 'node:path'

import { invalidArgument, otherError } from './errors.js'
import { dataLocalDir } from './platform.js'
import type { ApiKeySource, Config } from './types.js'
import type { CredentialStore } from './auth.js'

export const DEFAULT_BASE_URL = 'https://api.polaris.supply'
export const DEFAULT_CONCURRENCY = 4
export const DEFAULT_TIMEOUT_SECS = 60
export const ROOT_ENV_VAR = 'POLARIS_ROOT'
export const LEGACY_ROOT_ENV_VAR = 'TICK_ROOT'
export const CONCURRENCY_ENV_VAR = 'POLARIS_CONCURRENCY'
export const LEGACY_CONCURRENCY_ENV_VAR = 'TICK_CONCURRENCY'
export const TIMEOUT_ENV_VAR = 'POLARIS_TIMEOUT_SECS'
export const LEGACY_TIMEOUT_ENV_VAR = 'TICK_TIMEOUT_SECS'

export async function loadConfig(
  reader: (key: string) => string | undefined = (key) => process.env[key],
  store?: CredentialStore,
): Promise<Config> {
  const baseUrl =
    reader('POLARIS_BASE_URL')?.trim().replace(/\/+$/, '') || DEFAULT_BASE_URL

  const envApiKey = trimValue(reader('POLARIS_API_KEY'))
  const storedApiKey = !envApiKey && store ? await store.getApiKey() : undefined

  let apiKey: string | undefined
  let apiKeySource: ApiKeySource | undefined
  if (envApiKey) {
    apiKey = envApiKey
    apiKeySource = 'environment'
  } else if (storedApiKey) {
    apiKey = storedApiKey
    apiKeySource = 'credential_store'
  }

  const root =
    preferredEnv(reader, ROOT_ENV_VAR, LEGACY_ROOT_ENV_VAR)?.trim() || (await defaultRoot())

  const concurrency = parsePositiveNumber(
    preferredEnv(reader, CONCURRENCY_ENV_VAR, LEGACY_CONCURRENCY_ENV_VAR),
    DEFAULT_CONCURRENCY,
    CONCURRENCY_ENV_VAR,
  )
  const timeoutSecs = parsePositiveNumber(
    preferredEnv(reader, TIMEOUT_ENV_VAR, LEGACY_TIMEOUT_ENV_VAR),
    DEFAULT_TIMEOUT_SECS,
    TIMEOUT_ENV_VAR,
  )

  return compactOptional({
    baseUrl,
    apiKey,
    apiKeySource,
    root,
    concurrency,
    timeoutMs: timeoutSecs * 1000,
  })
}

export async function defaultRoot(): Promise<string> {
  const primary = dataLocalDir('polaris')
  const legacy = dataLocalDir('tick')
  return selectDefaultRoot(primary, legacy)
}

export function selectDefaultRoot(primaryRoot: string, legacyRoot: string): string {
  if (fs.existsSync(primaryRoot) || !fs.existsSync(legacyRoot)) {
    return primaryRoot
  }
  return legacyRoot
}

export function looksLikeCargoTargetDir(candidate: string): boolean {
  const parts = path.normalize(candidate).split(path.sep)
  let sawTarget = false
  for (const part of parts) {
    if (part === 'target') {
      sawTarget = true
      continue
    }
    if (sawTarget && (part === 'debug' || part === 'release')) return true
  }
  return false
}

function preferredEnv(
  reader: (key: string) => string | undefined,
  primary: string,
  legacy: string,
): string | undefined {
  return reader(primary) ?? reader(legacy)
}

function trimValue(value: string | undefined): string | undefined {
  const trimmed = value?.trim()
  return trimmed ? trimmed : undefined
}

function parsePositiveNumber(raw: string | undefined, fallback: number, name: string): number {
  if (!raw) return fallback
  const parsed = Number.parseInt(raw.trim(), 10)
  if (!Number.isFinite(parsed)) {
    throw otherError(`failed to parse ${name}`)
  }
  if (parsed <= 0) {
    throw invalidArgument(`${name} must be greater than zero`)
  }
  return parsed
}

function compactOptional<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  ) as T
}
