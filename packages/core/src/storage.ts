import fs from 'node:fs/promises'
import path from 'node:path'

import { otherError } from './errors.js'
import type { AccountIdentity, BookmarkStore } from './types.js'

function bookmarksPath(root: string): string {
  return path.join(root, 'bookmarks.json')
}

function accountIdentityPath(root: string): string {
  return path.join(root, 'account', 'identity.json')
}

export async function loadBookmarks(root: string): Promise<Set<string>> {
  const filePath = bookmarksPath(root)
  try {
    const contents = await fs.readFile(filePath, 'utf8')
    const store = JSON.parse(contents) as BookmarkStore
    return new Set(store.bookmarks ?? [])
  } catch (error) {
    const err = error as NodeJS.ErrnoException
    if (err.code === 'ENOENT') return new Set()
    throw otherError(`failed to parse ${filePath}`, error)
  }
}

export async function saveBookmarks(root: string, bookmarks: Set<string>): Promise<void> {
  await fs.mkdir(root, { recursive: true })
  const filePath = bookmarksPath(root)
  await fs.writeFile(
    filePath,
    JSON.stringify({ bookmarks: [...bookmarks].sort() }, null, 2),
    'utf8',
  )
}

export async function clearBookmarks(root: string): Promise<void> {
  const filePath = bookmarksPath(root)
  try {
    await fs.access(filePath)
  } catch {
    return
  }
  await saveBookmarks(root, new Set())
}

export async function loadAccountIdentity(root: string): Promise<AccountIdentity | undefined> {
  const filePath = accountIdentityPath(root)
  try {
    return JSON.parse(await fs.readFile(filePath, 'utf8')) as AccountIdentity
  } catch (error) {
    const err = error as NodeJS.ErrnoException
    if (err.code === 'ENOENT') return undefined
    throw otherError(`failed to parse ${filePath}`, error)
  }
}

export async function saveAccountIdentity(
  root: string,
  identity: AccountIdentity,
): Promise<void> {
  const filePath = accountIdentityPath(root)
  await fs.mkdir(path.dirname(filePath), { recursive: true })
  await fs.writeFile(filePath, JSON.stringify(identity, null, 2), 'utf8')
}
