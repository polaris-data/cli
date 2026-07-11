import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'

import { looksLikeCargoTargetDir } from './config.js'
import { otherError } from './errors.js'

export const UPDATE_INSTALLER_URL =
  'https://raw.githubusercontent.com/polaris-data/cli/main/install.sh'

export function inferInstallDirFromExecutable(executable: string): string | undefined {
  const basename = path.basename(executable)
  if (basename !== 'polaris' && basename !== 'tick') return undefined
  const installDir = path.dirname(executable)
  if (looksLikeCargoTargetDir(installDir)) return undefined
  return installDir
}

export async function createUpdateTempDir(): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `polaris-update-${process.pid}-`))
}

export async function downloadUpdateInstaller(targetPath: string): Promise<void> {
  const response = await fetch(UPDATE_INSTALLER_URL, {
    signal: AbortSignal.timeout(60_000),
  }).catch((error) => {
    throw otherError('failed to download install.sh', error)
  })
  if (!response.ok) throw otherError('failed to download install.sh')
  const body = await response.text()
  await fs.writeFile(targetPath, body, { mode: 0o755 })
  await fs.chmod(targetPath, 0o755)
}

export async function runUpdateInstaller(
  installerPath: string,
  version?: string,
  installDir?: string,
): Promise<number> {
  const args = [installerPath]
  if (version) args.push('--version', version)
  if (installDir) args.push('--install-dir', installDir)

  return new Promise((resolve, reject) => {
    const child = spawn('bash', args, {
      stdio: 'inherit',
    })
    child.on('error', (error) => reject(otherError('failed to execute install.sh', error)))
    child.on('close', (code) => resolve(code ?? 1))
  })
}
