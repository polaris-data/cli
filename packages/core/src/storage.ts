import fs from 'node:fs/promises'
import path from 'node:path'

function bookmarksPath(root: string): string {
  return path.join(root, 'bookmarks.json')
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
