import { spawn } from 'node:child_process'

import { otherError } from './errors.js'

export async function openUrl(url: string): Promise<void> {
  const [command, args] =
    process.platform === 'darwin'
      ? ['open', [url]]
      : process.platform === 'win32'
        ? ['explorer', [url]]
        : ['xdg-open', [url]]

  await spawnDetached(command, args, `failed to launch browser for ${url}`)
}

async function spawnDetached(command: string, args: string[], context: string): Promise<void> {
  try {
    const child = spawn(command, args, {
      detached: true,
      stdio: 'ignore',
    })
    child.unref()
  } catch (error) {
    throw otherError(context, error)
  }
}
