import { KeychainCredentialStore, PolarisClient, delay, invalidArgument, openUrl } from '@polaris/core'
import type { Config } from '@polaris/core'

import type { LoginOutput } from '../schemas.js'

const MIN_CLI_AUTH_POLL_INTERVAL_MS = 250

export async function runLoginCommand(config: Config): Promise<{
  human: string
  json: LoginOutput
}> {
  const client = new PolarisClient(config.baseUrl, undefined, config.timeoutMs)
  const start = await client.startCliAuth()

  const lines = [
    'Polaris login',
    `Base URL: ${config.baseUrl}`,
    `Code: ${start.user_code}`,
    `Browser: ${start.login_url}`,
  ]

  try {
    await openUrl(start.login_url)
    lines.push('Opened browser. Finish login there to continue.')
  } catch (error) {
    lines.push('Open the URL above manually to continue.')
    if (error instanceof Error) lines.push(error.message)
  }

  while (true) {
    const poll = await client.pollCliAuth(start.request_id, start.poll_token)
    if (poll.status === 'pending') {
      await delay(Math.max(poll.interval_ms, MIN_CLI_AUTH_POLL_INTERVAL_MS))
      continue
    }
    if (poll.status === 'approved') {
      const store = new KeychainCredentialStore()
      await store.setApiKey(poll.api_key)
      const signedInAs = poll.display_name ?? poll.email ?? poll.user_id
      lines.push(`Signed in as ${signedInAs}.`)

      let plan: string | null = null
      try {
        const accountClient = new PolarisClient(config.baseUrl, poll.api_key, config.timeoutMs)
        const account = await accountClient.fetchAccount()
        plan = account.subscription.tier
        lines.push(`Plan: ${plan}`)
      } catch {
        // Keep parity with Rust best-effort fetch.
      }

      return {
        human: lines.join('\n'),
        json: {
          status: 'signed_in',
          user_id: poll.user_id,
          display_name: poll.display_name ?? null,
          email: poll.email ?? null,
          plan,
        },
      }
    }
    if (poll.status === 'consumed') throw invalidArgument('login session was already consumed')
    throw invalidArgument('login session expired')
  }
}
