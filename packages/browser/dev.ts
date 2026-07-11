#!/usr/bin/env node
import { runPolarisBrowser } from './src/index.tsx'
import { PolarisClient, loadConfig } from '@polaris/core'
import { KeychainCredentialStore } from '@polaris/core'

async function main() {
  const store = new KeychainCredentialStore()
  const config = await loadConfig((key) => process.env[key], store)
  const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)

  // Parse command line arguments for browser seeds
  const args = process.argv.slice(2)
  const seed: { source?: string; market?: string; search?: string } = {}

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--source' && args[i + 1]) {
      seed.source = args[i + 1]
      i++
    } else if (args[i] === '--market' && args[i + 1]) {
      seed.market = args[i + 1]
      i++
    } else if (args[i] === '--search' && args[i + 1]) {
      seed.search = args[i + 1]
      i++
    }
  }

  await runPolarisBrowser(client, seed)
}

main().catch(console.error)
