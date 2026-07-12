import os from 'node:os'
import path from 'node:path'

export function dataLocalDir(appName: string): string {
  const home = os.homedir()

  switch (process.platform) {
    case 'darwin':
      return path.join(home, 'Library', 'Application Support', appName)
    case 'win32':
      return path.join(
        process.env.LOCALAPPDATA ?? path.join(home, 'AppData', 'Local'),
        appName,
      )
    default:
      return path.join(process.env.XDG_DATA_HOME ?? path.join(home, '.local', 'share'), appName)
  }
}
