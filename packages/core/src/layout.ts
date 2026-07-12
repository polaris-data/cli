import fs from 'node:fs/promises'
import path from 'node:path'
import { createHash } from 'node:crypto'

import { invalidArgument } from './errors.js'
import type { LocalSnapshotEntry } from './types.js'

export class Layout {
  constructor(readonly root: string) {}

  dataPathForKey(key: string): string {
    const [tier, source, market, date] = parseOpaqueKey(key)
    return path.join(this.root, 'data', tier, source, market, date, `${key}.jsonl.zst`)
  }

  tempPathForKey(key: string): string {
    const digest = createHash('sha256').update(key).digest('hex')
    return path.join(this.root, 'tmp', `${digest}.part`)
  }

  lockPath(): string {
    return path.join(this.root, 'locks', 'sync.lock')
  }

  cacheRoot(): string {
    return path.join(this.root, 'cache')
  }

  catalogCachePath(source: string, market: string): string {
    return path.join(this.cacheRoot(), 'catalog', source, `${market}.json`)
  }

  dataRoot(): string {
    return path.join(this.root, 'data')
  }

  tmpRoot(): string {
    return path.join(this.root, 'tmp')
  }

  async listLocalSnapshots(): Promise<LocalSnapshotEntry[]> {
    const dataRoot = this.dataRoot()
    try {
      await fs.access(dataRoot)
    } catch {
      return []
    }

    const files: LocalSnapshotEntry[] = []
    await collectSnapshotFiles(dataRoot, dataRoot, files)
    files.sort((left, right) => left.key.localeCompare(right.key))
    return files
  }
}

export function parseOpaqueKey(key: string): [string, string, string, string] {
  const trimmed = key.trim()
  if (!trimmed) throw invalidArgument('snapshot key must not be empty')

  const dateStart = findDatePattern(trimmed)
  if (dateStart === undefined) {
    throw invalidArgument(`opaque key does not contain a date: ${key}`)
  }
  if (dateStart === 0 || trimmed.charCodeAt(dateStart - 1) !== 45) {
    throw invalidArgument(`opaque key has unexpected format: ${key}`)
  }

  const date = trimmed.slice(dateStart, dateStart + 10)
  const prefix = trimmed.slice(0, dateStart - 1)
  const firstDash = prefix.indexOf('-')
  const secondDash = firstDash === -1 ? -1 : prefix.indexOf('-', firstDash + 1)
  if (firstDash <= 0 || secondDash <= firstDash + 1 || secondDash >= prefix.length - 1) {
    throw invalidArgument(`invalid opaque key prefix: ${key}`)
  }
  return [
    prefix.slice(0, firstDash),
    prefix.slice(firstDash + 1, secondDash),
    prefix.slice(secondDash + 1),
    date,
  ]
}

export function findDatePattern(text: string): number | undefined {
  for (let i = 0; i <= text.length - 10; i += 1) {
    const candidate = text.slice(i, i + 10)
    if (/^\d{4}-\d{2}-\d{2}$/.test(candidate) && !Number.isNaN(Date.parse(`${candidate}T00:00:00Z`))) {
      return i
    }
  }
  return undefined
}

export function inferDateFromText(text: string): string | undefined {
  const index = findDatePattern(text)
  return index === undefined ? undefined : text.slice(index, index + 10)
}

async function collectSnapshotFiles(
  root: string,
  current: string,
  files: LocalSnapshotEntry[],
): Promise<void> {
  const entries = await fs.readdir(current, { withFileTypes: true })
  for (const entry of entries) {
    const filePath = path.join(current, entry.name)
    if (entry.isDirectory()) {
      await collectSnapshotFiles(root, filePath, files)
      continue
    }
    if (!entry.isFile()) continue

    const relativePath = path.relative(root, filePath).split(path.sep).join('/')
    const filename = path.basename(filePath)
    const key = filename.endsWith('.jsonl.zst') ? filename.slice(0, -10) : filename
    const [source, market, date] = inferLocalMetadata(relativePath)
    files.push(compactOptional({
      key,
      path: filePath,
      filename,
      source,
      market,
      date,
    }))
  }
}

export function inferLocalMetadata(
  relativePath: string,
): [string | undefined, string | undefined, string | undefined] {
  const filename = relativePath.split('/').at(-1)
  if (filename) {
    const key = filename.endsWith('.jsonl.zst') ? filename.slice(0, -10) : filename
    try {
      const [, source, market, date] = parseOpaqueKey(key)
      return [source, market, date]
    } catch {
      // Fall through to directory inference.
    }
  }

  const segments = relativePath.split('/')
  if (segments.length >= 5) {
    return [segments[1], segments[2], segments[3]]
  }
  return [undefined, undefined, undefined]
}

function compactOptional<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  ) as T
}
