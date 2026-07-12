#!/usr/bin/env node
import { Cli, z } from 'incur'
import { spawn } from 'node:child_process'
import fs from 'node:fs/promises'
import path from 'node:path'
import { createInterface } from 'node:readline/promises'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

import {
  KeychainCredentialStore,
  Layout,
  PolarisClient,
  acquireSyncLock,
  buildSyncPlan,
  clearBookmarks,
  executeSync,
  invalidArgument,
  loadConfig,
  openUrl,
  parseRfc3339,
  presentTotal,
  remoteTotal,
  type CatalogMarket,
  type Config,
  type LocalSnapshotEntry,
  type SyncExecution,
  type SyncPlan,
} from '@polaris/core'

const version = '0.8.0'
const MIN_CLI_AUTH_POLL_INTERVAL_MS = 250

const remoteDatasetSchema = z
  .object({
    source: z.string(),
    market: z.string(),
    start: z.string(),
    end: z.string(),
    catalog_source: z.string().nullable(),
    access: z
      .object({
        status: z.enum(['open', 'preview', 'restricted']),
        public_cutoff_date: z.string().nullable(),
      })
      .nullable(),
    categories: z.array(z.string()).optional(),
    dataset: z.string(),
  })
  .transform((value) => value)

const remoteListOutputSchema = z.object({
  command: z.literal('catalog'),
  filters: z.object({
    source: z.string().nullable(),
    market: z.string().nullable(),
    search: z.string().nullable(),
  }),
  dataset_total: z.number(),
  datasets: z.array(remoteDatasetSchema),
})

const localSnapshotSchema = z.object({
  key: z.string(),
  path: z.string(),
  filename: z.string(),
  source: z.string().nullable(),
  market: z.string().nullable(),
  date: z.string().nullable(),
  start: z.null(),
  end: z.null(),
})

const localListOutputSchema = z.object({
  command: z.literal('list'),
  root: z.string(),
  filters: z.object({
    source: z.string().nullable(),
    market: z.string().nullable(),
    date: z.string().nullable(),
  }),
  snapshot_total: z.number(),
  snapshots: z.array(localSnapshotSchema),
})

const syncOutputSchema = z.object({
  command: z.literal('download'),
  source: z.string(),
  market: z.string(),
  requested_range: z.object({ from: z.string(), to: z.string() }),
  effective_range: z.object({ from: z.string(), to: z.string() }),
  root: z.string(),
  remote_total: z.number(),
  downloaded_total: z.number(),
  skipped_total: z.number(),
  failed_total: z.number(),
  downloaded_keys: z.array(z.string()),
  failed: z.array(z.object({ key: z.string(), error: z.string() })),
})

const resetOutputSchema = z.object({
  command: z.literal('reset'),
  root: z.string(),
  snapshot_total: z.number(),
  removed_roots: z.array(z.string()),
})

const updateOutputSchema = z.object({
  command: z.literal('update'),
  install_script: z.string(),
  runtime_dir: z.string(),
  install_dir: z.string().nullable(),
  version: z.string().nullable(),
  status: z.literal('updated'),
})

type RemoteDatasetEntry = z.infer<typeof remoteDatasetSchema>
type RemoteListOutput = z.infer<typeof remoteListOutputSchema>
type LocalListOutput = z.infer<typeof localListOutputSchema>
type SyncOutput = z.infer<typeof syncOutputSchema>
type ResetOutput = z.infer<typeof resetOutputSchema>
type UpdateOutput = z.infer<typeof updateOutputSchema>

export const cli = Cli.create('polaris', {
  version,
  description: 'Download Polaris market data snapshots',
  async run(c) {
    const config = await loadRuntimeConfig()
    const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
    if (canRenderBrowser(c.formatExplicit)) {
      const spec: string = '@polaris/browser'
      const { runPolarisBrowser } = (await import(spec)) as {
        runPolarisBrowser: (
          client: PolarisClient,
          seed?: { source?: string; market?: string; search?: string },
        ) => Promise<number>
      }
      await runPolarisBrowser(client, {})
      return
    }
    const result = await runCatalogCommand(config, client, {
      source: null,
      market: null,
      search: null,
      limit: Number.MAX_SAFE_INTEGER,
    })
    return formatCommandResult(c.formatExplicit, result.output, renderRemoteListOutput(result.output))
  },
})

cli.command('account', {
  description: 'Print the current Polaris auth state and account details.',
  output: z.union([z.string(), z.any()]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
      const result = await runAccountCommand(config, client)
      return formatCommandResult(c.formatExplicit, result.json, result.human)
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('catalog', {
  description: 'List remote datasets available from Polaris.',
  options: z.object({
    source: z.string().optional(),
    market: z.string().optional(),
    search: z.string().optional(),
    limit: z.coerce.number().default(100),
  }),
  output: z.union([z.string(), remoteListOutputSchema]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
      const result = await runCatalogCommand(config, client, {
        source: c.options.source ?? null,
        market: c.options.market ?? null,
        search: c.options.search ?? null,
        limit: c.options.limit,
      })
      const cta = result.output.datasets[0]
        ? {
            commands: [
              {
                command: 'download',
                options: {
                  source: result.output.datasets[0].source,
                  market: result.output.datasets[0].market,
                  from: result.output.datasets[0].start,
                  to: result.output.datasets[0].end,
                },
                description: 'Download the first listed dataset coverage.',
              },
            ],
          }
        : undefined
      return formatCommandResult(
        c.formatExplicit,
        result.output,
        renderRemoteListOutput(result.output),
        cta ? { cta } : undefined,
      )
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('feedback', {
  description: 'Send product feedback to the Polaris team.',
  args: z.object({
    message: z.string(),
  }),
  output: z.union([z.string(), z.object({ ok: z.literal(true) })]),
  async run(c) {
    try {
      const message = c.args.message.trim()
      if (!message) throw invalidArgument('feedback message cannot be empty')
      const config = await loadRuntimeConfig()
      const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
      const response = await client.submitFeedback(message)
      if (!response.ok) throw new Error('feedback request failed: API returned ok=false')
      return formatCommandResult(c.formatExplicit, { ok: true as const }, 'Feedback sent.')
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('key', {
  description: 'Store a Polaris API key from a secure prompt.',
  output: z.union([z.string(), z.object({ stored: z.literal(true) })]),
  async run(c) {
    try {
      const apiKey = await promptPassword('Polaris API key: ')
      const store = new KeychainCredentialStore()
      await store.setApiKey(apiKey)
      return formatCommandResult(
        c.formatExplicit,
        { stored: true as const },
        'Stored Polaris API key in persistent credential storage.',
      )
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('login', {
  description: 'Sign in through the browser and store the returned API key.',
  output: z.union([
    z.string(),
    z.object({
      status: z.literal('signed_in'),
      user_id: z.string(),
      display_name: z.string().nullable(),
      email: z.string().nullable(),
      plan: z.string().nullable(),
    }),
  ]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const result = await runLoginCommand(config)
      return formatCommandResult(
        c.formatExplicit,
        result.json,
        result.human,
        result.json
          ? {
              cta: {
                commands: [{ command: 'account', description: 'Check the signed-in account.' }],
              },
            }
          : undefined,
      )
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('list', {
  description: 'List local snapshots under the configured root.',
  options: z.object({
    source: z.string().optional(),
    market: z.string().optional(),
    date: z.string().optional(),
  }),
  output: z.union([z.string(), localListOutputSchema]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const output = await runListCommand(config, {
        source: c.options.source ?? null,
        market: c.options.market ?? null,
        date: c.options.date ?? null,
      })
      return formatCommandResult(c.formatExplicit, output, renderLocalListOutput(output))
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('download', {
  description: 'Download missing snapshots for a dataset and time range.',
  options: z.object({
    source: z.string(),
    market: z.string(),
    from: z.string(),
    to: z.string(),
    concurrency: z.coerce.number().optional(),
  }),
  output: z.union([z.string(), syncOutputSchema]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
      const result = await runDownloadCommand(config, client, compactOptional({
        source: c.options.source,
        market: c.options.market,
        from: c.options.from,
        to: c.options.to,
        concurrency: c.options.concurrency,
      }))
      return formatCommandResult(
        c.formatExplicit,
        result.output,
        renderSyncOutput(result.output),
        result.exitCode === 0
          ? undefined
          : {
              cta: {
                commands: [{ command: 'catalog', description: 'Inspect dataset coverage again.' }],
              },
            },
      )
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('reset', {
  description: 'Remove all local dataset state managed by Polaris.',
  output: z.union([z.string(), resetOutputSchema]),
  async run(c) {
    try {
      const config = await loadRuntimeConfig()
      const output = await runResetCommand(config)
      return formatCommandResult(c.formatExplicit, output, renderResetOutput(output))
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

cli.command('update', {
  description: 'Reinstall or update Polaris using the bundled installer.',
  options: z.object({
    version: z.string().optional(),
  }),
  output: z.union([z.string(), updateOutputSchema]),
  async run(c) {
    try {
      const output = await runUpdateCommand({ version: c.options.version ?? null })
      return formatCommandResult(c.formatExplicit, output, renderUpdateOutput(output))
    } catch (error) {
      return handleCliError(c, error)
    }
  },
})

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await cli.serve(process.argv.slice(2))
}

async function loadRuntimeConfig(): Promise<Config> {
  return loadConfig((key) => process.env[key], new KeychainCredentialStore())
}

function canRenderBrowser(formatExplicit: boolean): boolean {
  return !formatExplicit && process.stdout.isTTY === true && process.stdin.isTTY === true
}

async function runAccountCommand(config: Config, client: PolarisClient): Promise<{
  human: string
  json?: {
    base_url: string
    auth: string
    status: string
    user_id: string | null
    email: string | null
    plan: string | null
    provider: string | null
    key_id: string | null
  }
}> {
  const authSource =
    config.apiKeySource === 'environment'
      ? 'configured via POLARIS_API_KEY'
      : config.apiKeySource === 'credential_store'
        ? 'configured via stored credential'
        : 'not configured'

  if (!config.apiKey) {
    return {
      human: ['Polaris account', `Base URL: ${config.baseUrl}`, `Auth: ${authSource}`, 'Status: not signed in', 'Run `polaris login` to sign in.'].join('\n'),
      json: {
        base_url: config.baseUrl,
        auth: authSource,
        status: 'not signed in',
        user_id: null,
        email: null,
        plan: null,
        provider: null,
        key_id: null,
      },
    }
  }

  const account = await client.fetchAccount()
  const displayName = account.identity.display_name ?? account.identity.email ?? account.user_id
  const lines = [
    'Polaris account',
    `Base URL: ${config.baseUrl}`,
    `Auth: ${authSource}`,
    `Status: signed in as ${displayName}`,
    `User ID: ${account.user_id}`,
  ]
  if (account.identity.email) lines.push(`Email: ${account.identity.email}`)
  lines.push(`Plan: ${account.subscription.tier}`)
  lines.push(`Provider: ${account.auth.provider}`)
  if (account.auth.key_id) lines.push(`Key ID: ${account.auth.key_id}`)

  return {
    human: lines.join('\n'),
    json: {
      base_url: config.baseUrl,
      auth: authSource,
      status: `signed in as ${displayName}`,
      user_id: account.user_id,
      email: account.identity.email ?? null,
      plan: account.subscription.tier,
      provider: account.auth.provider,
      key_id: account.auth.key_id ?? null,
    },
  }
}

async function runCatalogCommand(
  config: Config,
  client: PolarisClient,
  filters: { source: string | null; market: string | null; search: string | null; limit: number },
): Promise<{ output: RemoteListOutput }> {
  if (filters.limit <= 0) throw invalidArgument('--limit must be greater than zero')
  const catalog = await client.fetchCatalog(filters.source ?? undefined, filters.market ?? undefined)
  const datasets = filterRemoteCatalog(catalog.markets, filters, filters.limit)
  return {
    output: {
      command: 'catalog',
      filters,
      dataset_total: datasets.length,
      datasets,
    },
  }
}

async function runListCommand(
  config: Config,
  filters: { source: string | null; market: string | null; date: string | null },
): Promise<LocalListOutput> {
  const entries = await new Layout(config.root).listLocalSnapshots()
  const snapshots = entries
    .filter((entry) => matchesExact(entry.source ?? null, filters.source))
    .filter((entry) => matchesExact(entry.market ?? null, filters.market))
    .filter((entry) => matchesExact(entry.date ?? null, filters.date))
    .map((entry) => toLocalSnapshotJson(entry))

  return {
    command: 'list',
    root: config.root,
    filters,
    snapshot_total: snapshots.length,
    snapshots,
  }
}

async function runDownloadCommand(
  config: Config,
  client: PolarisClient,
  options: {
    source: string
    market: string
    from: string
    to: string
    concurrency?: number | undefined
  },
): Promise<{ output: SyncOutput; exitCode: number }> {
  const requestedRange = {
    from: parseRfc3339(options.from, '--from').toISOString(),
    to: parseRfc3339(options.to, '--to').toISOString(),
  }
  if (requestedRange.from > requestedRange.to) {
    throw invalidArgument('--from must be less than or equal to --to')
  }

  const layout = new Layout(config.root)
  const guard = await acquireSyncLock(layout)
  try {
    const plan = await buildSyncPlan(client, config, options.source, options.market, requestedRange)
    const concurrency = options.concurrency ?? config.concurrency
    if (concurrency <= 0) throw invalidArgument('--concurrency must be greater than zero')
    const execution = await executeSync(client, plan, concurrency)
    const output = toSyncOutput(plan, execution)
    return { output, exitCode: output.failed_total > 0 ? 1 : 0 }
  } finally {
    await guard.release()
  }
}

async function runResetCommand(config: Config): Promise<ResetOutput> {
  const layout = new Layout(config.root)
  const guard = await acquireSyncLock(layout)
  try {
    const snapshotTotal = (await layout.listLocalSnapshots()).length
    const candidateRoots = [layout.dataRoot(), layout.tmpRoot(), layout.cacheRoot()]
    const removedRoots: string[] = []
    for (const root of candidateRoots) {
      try {
        await fs.rm(root, { recursive: true })
        removedRoots.push(root)
      } catch (error) {
        const err = error as NodeJS.ErrnoException
        if (err.code !== 'ENOENT') throw error
      }
    }
    await clearBookmarks(config.root)
    return {
      command: 'reset',
      root: config.root,
      snapshot_total: snapshotTotal,
      removed_roots: removedRoots,
    }
  } finally {
    await guard.release()
  }
}

async function runUpdateCommand(options: { version: string | null }): Promise<UpdateOutput> {
  const runtimeDir = resolveCliRuntimeRoot()
  const installScript = path.join(runtimeDir, 'install.sh')
  const installDir = inferInstallDirFromRuntimeRoot(runtimeDir)

  try {
    await fs.access(installScript)
  } catch {
    throw invalidArgument(`installer not found at ${installScript}`)
  }

  const args = [installScript, '--runtime-dir', runtimeDir]
  if (installDir) args.push('--install-dir', installDir)
  if (options.version) args.push('--version', options.version)

  await runInstallerScript('bash', args)

  return {
    command: 'update',
    install_script: installScript,
    runtime_dir: runtimeDir,
    install_dir: installDir,
    version: options.version,
    status: 'updated',
  }
}

async function runLoginCommand(config: Config): Promise<{
  human: string
  json: {
    status: 'signed_in'
    user_id: string
    display_name: string | null
    email: string | null
    plan: string | null
  }
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
      await sleep(Math.max(poll.interval_ms, MIN_CLI_AUTH_POLL_INTERVAL_MS))
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

function filterRemoteCatalog(
  markets: CatalogMarket[],
  filters: { source: string | null; market: string | null; search: string | null },
  limit: number,
): RemoteDatasetEntry[] {
  const datasets = markets
    .filter((market) => matchesExact(market.source, filters.source))
    .filter((market) => matchesExact(market.market, filters.market))
    .map((market) => toRemoteDatasetEntry(market))
    .filter((entry) => matchesSearch(entry, filters.search))
    .sort(
      (left, right) =>
        accessSortOrder(left.access) - accessSortOrder(right.access) ||
        left.dataset.localeCompare(right.dataset),
    )

  return datasets.slice(0, limit)
}

function toRemoteDatasetEntry(market: CatalogMarket): RemoteDatasetEntry {
  const entry: RemoteDatasetEntry = {
    source: market.source,
    market: market.market,
    start: market.start,
    end: market.end,
    catalog_source: market.catalog_source ?? null,
    access: market.access
      ? {
          status: market.access.status,
          public_cutoff_date: market.access.public_cutoff_date ?? null,
        }
      : null,
    dataset: `${market.source}:${market.market}`,
  }
  if (market.categories.length > 0) entry.categories = market.categories
  return entry
}

function matchesSearch(entry: RemoteDatasetEntry, search: string | null): boolean {
  const normalized = search?.trim().toLowerCase()
  if (!normalized) return true
  const haystack = [
    entry.dataset,
    entry.catalog_source ?? '',
    ...(entry.categories ?? []),
    accessSummary(entry.access),
  ]
    .join(' ')
    .toLowerCase()
  return normalized.split(/\s+/).every((token) => haystack.includes(token))
}

function accessSortOrder(
  access: RemoteDatasetEntry['access'],
): number {
  if (!access) return Number.MAX_SAFE_INTEGER
  switch (access.status) {
    case 'open':
      return 0
    case 'preview':
      return 1
    case 'restricted':
      return 2
  }
}

function accessSummary(access: RemoteDatasetEntry['access']): string {
  if (!access) return 'unknown'
  if (access.status === 'preview' && access.public_cutoff_date) {
    return `preview from ${access.public_cutoff_date}`
  }
  return access.status
}

function toLocalSnapshotJson(entry: LocalSnapshotEntry) {
  return {
    key: entry.key,
    path: entry.path,
    filename: entry.filename,
    source: entry.source ?? null,
    market: entry.market ?? null,
    date: entry.date ?? null,
    start: null,
    end: null,
  }
}

function toSyncOutput(plan: SyncPlan, execution: SyncExecution): SyncOutput {
  return {
    command: 'download',
    source: plan.source,
    market: plan.market,
    requested_range: plan.requestedRange,
    effective_range: plan.effectiveRange,
    root: plan.root,
    remote_total: remoteTotal(plan),
    downloaded_total: execution.downloadedKeys.length,
    skipped_total: presentTotal(plan),
    failed_total: execution.failed.length,
    downloaded_keys: execution.downloadedKeys,
    failed: execution.failed,
  }
}

function renderRemoteListOutput(output: RemoteListOutput): string {
  const lines = ['catalog']
  if (output.filters.source || output.filters.market || output.filters.search) {
    lines.push(
      `filters: source=${formatMaybe(output.filters.source)} market=${formatMaybe(output.filters.market)} search=${formatMaybe(output.filters.search)}`,
    )
  }
  lines.push(`datasets: ${output.dataset_total}`)
  if (output.datasets.length > 0) {
    lines.push('remote datasets:')
    for (const dataset of output.datasets) {
      lines.push(
        `  ${dataset.source}:${dataset.market} ${dataset.start} -> ${dataset.end} (${accessSummary(dataset.access)})`,
      )
    }
  }
  return lines.join('\n')
}

function renderLocalListOutput(output: LocalListOutput): string {
  const lines = ['list', `root: ${output.root}`]
  if (output.filters.source || output.filters.market || output.filters.date) {
    lines.push(
      `filters: source=${formatMaybe(output.filters.source)} market=${formatMaybe(output.filters.market)} date=${formatMaybe(output.filters.date)}`,
    )
  }
  lines.push(`snapshots: ${output.snapshot_total}`)
  if (output.snapshots.length > 0) {
    lines.push('local snapshots:')
    for (const snapshot of output.snapshots.slice(0, 50)) lines.push(`  ${snapshot.key}`)
    if (output.snapshots.length > 50) {
      lines.push(`  ... ${output.snapshots.length - 50} more`)
    }
  }
  return lines.join('\n')
}

function renderSyncOutput(output: SyncOutput): string {
  const lines = [
    `download ${output.source} ${output.market}`,
    `root: ${output.root}`,
    `requested: ${output.requested_range.from} -> ${output.requested_range.to}`,
    `effective: ${output.effective_range.from} -> ${output.effective_range.to}`,
    `remote: ${output.remote_total}`,
    `downloaded: ${output.downloaded_total}`,
    `skipped: ${output.skipped_total}`,
    `failed: ${output.failed_total}`,
  ]
  if (output.failed.length > 0) {
    lines.push('failed keys:')
    for (const failure of output.failed) lines.push(`  ${failure.key}: ${failure.error}`)
  }
  return lines.join('\n')
}

function renderResetOutput(output: ResetOutput): string {
  const lines = ['reset', `root: ${output.root}`, `removed snapshots: ${output.snapshot_total}`]
  if (output.removed_roots.length > 0) {
    lines.push('removed roots:')
    for (const root of output.removed_roots) lines.push(`  ${root}`)
  }
  return lines.join('\n')
}

function renderUpdateOutput(output: UpdateOutput): string {
  const lines = [
    'update',
    `runtime: ${output.runtime_dir}`,
    `installer: ${output.install_script}`,
    `install dir: ${output.install_dir ?? 'default'}`,
  ]
  if (output.version) lines.push(`version: ${output.version}`)
  lines.push('status: updated')
  return lines.join('\n')
}

function formatCommandResult<T>(
  formatExplicit: boolean,
  jsonValue: T,
  human: string,
  meta?: { cta?: { commands: Array<Record<string, unknown>> } | undefined },
): T | string {
  if (formatExplicit) {
    return jsonValue
  }
  if (meta?.cta) {
    const next = meta.cta.commands
      .map((command) => {
        const name = String(command.command)
        const description = command.description ? ` - ${String(command.description)}` : ''
        return `  ${name}${description}`
      })
      .join('\n')
    return `${human}\nNext:\n${next}`
  }
  return human
}

function handleCliError(
  c: {
    error: (options: {
      code: string
      message: string
      retryable?: boolean
      exitCode?: number
    }) => never
  },
  error: unknown,
): never {
  if (error instanceof Error && 'kind' in error && 'exitCode' in error) {
    const exitCode = (error as { exitCode: () => number }).exitCode()
    const kind = String((error as { kind: string }).kind).toUpperCase()
    const retryable = Boolean((error as { retryable?: boolean }).retryable)
    return c.error({
      code: kind,
      message: error.message,
      retryable,
      exitCode,
    })
  }
  return c.error({
    code: 'OTHER',
    message: error instanceof Error ? error.message : String(error),
    exitCode: 1,
  })
}

function matchesExact(value: string | null, filter: string | null): boolean {
  return filter === null ? true : value === filter
}

function formatMaybe(value: string | null): string {
  return value === null ? 'None' : JSON.stringify(value)
}

async function promptPassword(prompt: string): Promise<string> {
  const rl = createInterface({
    input: process.stdin,
    output: process.stdout,
  })
  const mutable = rl as unknown as {
    _writeToOutput?: ((value: string) => void) | undefined
    line?: string | undefined
    output?: NodeJS.WritableStream | undefined
  }
  const original = mutable._writeToOutput
  mutable._writeToOutput = (value: string) => {
    if (mutable.line) {
      mutable.output?.write('*'.repeat(mutable.line.length))
      return
    }
    mutable.output?.write(value)
  }
  try {
    const answer = (await rl.question(prompt)).trim()
    if (!answer) throw invalidArgument('API key cannot be empty')
    mutable.output?.write('\n')
    return answer
  } finally {
    mutable._writeToOutput = original
    rl.close()
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function compactOptional<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  ) as T
}

function resolveCliRuntimeRoot(): string {
  const moduleDir = path.dirname(fileURLToPath(import.meta.url))
  return path.resolve(moduleDir, '../../../../..')
}

function inferInstallDirFromRuntimeRoot(runtimeDir: string): string | null {
  const normalized = path.normalize(runtimeDir)
  const suffixes = [
    path.join('.polaris', 'lib', 'polaris'),
    path.join('.tick', 'lib', 'polaris'),
    path.join('lib', 'polaris'),
  ]

  if (suffixes.some((suffix) => normalized.endsWith(suffix))) {
    return path.resolve(runtimeDir, '..', '..', 'bin')
  }

  return null
}

async function runInstallerScript(command: string, args: string[]): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: ['inherit', 'pipe', 'pipe'],
      env: process.env,
    })

    child.stdout.on('data', (chunk) => {
      process.stderr.write(chunk)
    })
    child.stderr.on('data', (chunk) => {
      process.stderr.write(chunk)
    })

    child.on('error', reject)
    child.on('close', (code) => {
      if (code === 0) resolve()
      else reject(new Error(`installer exited with status ${code ?? 'unknown'}`))
    })
  })
}
