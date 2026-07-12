import fs from 'node:fs/promises'
import path from 'node:path'

import keytar from 'keytar'

import { invalidArgument, otherError } from './errors.js'
import { dataLocalDir } from './platform.js'

const PRIMARY_SERVICE_NAME = 'polaris'
const LEGACY_SERVICE_NAME = 'tick'
const ACCOUNT_NAME = 'polaris-api-key'
const PRIMARY_APP_NAME = 'polaris'
const LEGACY_APP_NAME = 'tick'

export interface CredentialStore {
  getApiKey(): Promise<string | undefined>
  setApiKey(apiKey: string): Promise<void>
}

export class KeychainCredentialStore implements CredentialStore {
  async getApiKey(): Promise<string | undefined> {
    let readError: Error | undefined

    for (const service of [PRIMARY_SERVICE_NAME, LEGACY_SERVICE_NAME]) {
      try {
        const value = trimValue(await keytar.getPassword(service, ACCOUNT_NAME))
        if (value) return value
      } catch (error) {
        readError ??= otherError(
          `failed to read Polaris API key from OS credential store: ${String(error)}`,
          error,
        )
      }
    }

    for (const appName of [PRIMARY_APP_NAME, LEGACY_APP_NAME]) {
      const value = await readFallbackApiKey(appName)
      if (value) return value
    }

    if (readError) throw readError
    return undefined
  }

  async setApiKey(apiKey: string): Promise<void> {
    const trimmed = apiKey.trim()
    if (!trimmed) throw invalidArgument('API key cannot be empty')

    let keychainError: unknown
    try {
      await keytar.setPassword(PRIMARY_SERVICE_NAME, ACCOUNT_NAME, trimmed)
      try {
        await keytar.setPassword(LEGACY_SERVICE_NAME, ACCOUNT_NAME, trimmed)
      } catch {
        // Best effort legacy backfill.
      }
      const stored = trimValue(await keytar.getPassword(PRIMARY_SERVICE_NAME, ACCOUNT_NAME))
      if (stored !== trimmed) {
        keychainError = new Error(
          'stored Polaris API key could not be read back from OS credential store',
        )
      }
    } catch (error) {
      keychainError = error
    }

    try {
      await writeFallbackApiKey(PRIMARY_APP_NAME, trimmed)
    } catch (primaryError) {
      try {
        await writeFallbackApiKey(LEGACY_APP_NAME, trimmed)
      } catch (legacyError) {
        throw otherError(
          `failed to persist Polaris API key in fallback file stores: ${String(primaryError)}; ${String(legacyError)}`,
        )
      }
    }

    if (keychainError) {
      // File-backed storage is already written above, so preserve Rust semantics.
      console.warn(`falling back to file-backed Polaris API key storage: ${String(keychainError)}`)
    }
  }
}

export function credentialFallbackPath(appName: string): string {
  return path.join(dataLocalDir(appName), 'account', 'api-key.txt')
}

export async function readFallbackApiKey(appName: string): Promise<string | undefined> {
  const filePath = credentialFallbackPath(appName)
  try {
    return trimValue(await fs.readFile(filePath, 'utf8'))
  } catch (error) {
    const err = error as NodeJS.ErrnoException
    if (err.code === 'ENOENT') return undefined
    throw otherError(`failed to read fallback credential file ${filePath}`, error)
  }
}

export async function writeFallbackApiKey(appName: string, apiKey: string): Promise<void> {
  const filePath = credentialFallbackPath(appName)
  await fs.mkdir(path.dirname(filePath), { recursive: true })
  await fs.writeFile(filePath, `${apiKey}\n`, { mode: 0o600 })
}

function trimValue(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim()
  return trimmed ? trimmed : undefined
}
