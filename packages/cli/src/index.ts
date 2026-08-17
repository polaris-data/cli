#!/usr/bin/env node
import { Binary, Cli, Formatter, z } from 'incur'
import fsSync from 'node:fs'
import fs from 'node:fs/promises'
import path from 'node:path'
import { createInterface } from 'node:readline/promises'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

import packageJson from '../package.json' with { type: 'json' }
import {
  KeychainCredentialStore,
  PolarisClient,
  PolarisError,
  compactOptional,
  invalidArgument,
  loadConfig,
  type Config,
} from '@polaris/core'

import {
  accountOutputSchema,
  localListOutputSchema,
  loginOutputSchema,
  remoteListOutputSchema,
  resetOutputSchema,
  syncOutputSchema,
} from './schemas.js'
import { runAccountCommand } from './commands/account.js'
import { runCatalogCommand } from './commands/catalog.js'
import { runDownloadCommand } from './commands/download.js'
import { runListCommand } from './commands/list.js'
import { runLoginCommand } from './commands/login.js'
import { runResetCommand } from './commands/reset.js'
import {
  formatCommandResult,
  renderLocalListOutput,
  renderRemoteListOutput,
  renderResetOutput,
  renderSyncOutput,
} from './render/index.js'

const LATEST_RELEASE_URL = 'https://api.github.com/repos/polaris-data/cli/releases/latest'
const UPDATE_CHECK_TIMEOUT_MS = 10_000

const defaultMcpCommand = resolveDefaultMcpCommand()

export const cli = Cli.create('polaris', {
  version: resolveCliVersion(),
  update: Binary.github({ repository: 'polaris-data/cli' }),
  description:
    'Before using Polaris commands, read https://docs.polaris.supply/llms.txt for the docs map and workflow guidance. Use Polaris to browse and download market data snapshots.',
  hint: [
    'Start here: read https://docs.polaris.supply/llms.txt before using Polaris commands.',
    'Docs: https://docs.polaris.supply',
    'Python SDK: https://docs.polaris.supply/sdks/python',
    'TypeScript SDK: https://docs.polaris.supply/sdks/typescript',
    'Platform LLM reference: https://www.polaris.supply/llms.txt',
  ].join('\n'),
  mcp: {
    command: defaultMcpCommand,
  },
  sync: {
    depth: 0,
  },
  async run(c) {
    const config = await loadRuntimeConfig()
    const client = new PolarisClient(config.baseUrl, config.apiKey, config.timeoutMs)
    if (canRenderBrowser(c.formatExplicit)) {
      const { runPolarisBrowser } = await import('@polaris/browser')
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
  output: z.union([z.string(), accountOutputSchema]),
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
  output: z.union([z.string(), loginOutputSchema]),
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

export async function isDirectCliExecution(
  moduleUrl: string,
  entryArg: string | undefined,
): Promise<boolean> {
  if (!entryArg) return false

  try {
    const [modulePath, entryPath] = await Promise.all([
      fs.realpath(fileURLToPath(moduleUrl)),
      fs.realpath(entryArg),
    ])
    return modulePath === entryPath
  } catch {
    return path.resolve(fileURLToPath(moduleUrl)) === path.resolve(entryArg)
  }
}

export function resolveDefaultMcpCommand(
  entryArg: string | undefined = process.argv[1],
  runtimePath: string | undefined = process.execPath,
): string {
  const installedBinary = resolveInstalledPolarisBinary(entryArg)
  if (installedBinary) return `${quoteCommandArg(installedBinary)} --mcp`

  const nodeScript = resolveNodeScriptEntry(entryArg)
  if (nodeScript && runtimePath) {
    return `${quoteCommandArg(runtimePath)} ${quoteCommandArg(nodeScript)} --mcp`
  }

  return 'polaris --mcp'
}

export function resolveCliVersion(
  embeddedVersion: string | undefined = Binary.version,
  packageVersion: string = packageJson.version,
): string {
  return embeddedVersion ?? packageVersion
}

export async function maybeHandleAlreadyCurrentUpdate(
  argv: string[],
  options: {
    binaryTarget?: string | undefined
    binaryVersion?: string | undefined
    fetchLatest?: typeof globalThis.fetch | undefined
    isTty?: boolean | undefined
    stdout?: ((value: string) => void) | undefined
  } = {},
): Promise<boolean> {
  if (!argv.includes('--update') || argv.includes('--help') || argv.includes('-h')) return false

  const current = normalizeStableVersion(options.binaryVersion)
  if (!current || !options.binaryTarget) return false

  const outputFormat = resolveUpdateOutputFormat(argv)
  if (!outputFormat.valid) return false

  try {
    const response = await (options.fetchLatest ?? globalThis.fetch)(LATEST_RELEASE_URL, {
      headers: {
        accept: 'application/vnd.github+json',
        'user-agent': 'polaris',
        'x-github-api-version': '2022-11-28',
      },
      signal: AbortSignal.timeout(UPDATE_CHECK_TIMEOUT_MS),
    })
    if (!response.ok) return false

    const metadata: unknown = await response.json()
    if (!metadata || typeof metadata !== 'object') return false
    const latest = normalizeStableVersion((metadata as Record<string, unknown>).tag_name)
    if (latest !== current) return false
  } catch {
    return false
  }

  const result = { current, name: 'polaris', status: 'up_to_date' }
  const human = (options.isTty ?? process.stdout.isTTY === true) && !outputFormat.explicit
  const output = human
    ? `✓ polaris is already up to date (${current})`
    : Formatter.format(result, outputFormat.format)
  const stdout = options.stdout ?? ((value: string) => process.stdout.write(value))
  stdout(output.endsWith('\n') ? output : `${output}\n`)
  return true
}

function resolveInstalledPolarisBinary(entryArg: string | undefined): string | null {
  if (!entryArg) return null
  const resolved = resolvePathForCommand(entryArg)
  const normalized = path.basename(resolved).toLowerCase()
  return normalized === 'polaris' || normalized === 'polaris.cmd' || normalized === 'polaris.ps1'
    ? resolved
    : null
}

function resolveNodeScriptEntry(entryArg: string | undefined): string | null {
  if (!entryArg) return null
  const resolved = resolvePathForCommand(entryArg)
  const extension = path.extname(resolved).toLowerCase()
  return extension === '.js' || extension === '.mjs' || extension === '.cjs' ? resolved : null
}

function resolvePathForCommand(filePath: string): string {
  try {
    return fsSync.realpathSync(filePath)
  } catch {
    return path.resolve(filePath)
  }
}

function quoteCommandArg(value: string): string {
  return /[\s"]/.test(value) ? JSON.stringify(value) : value
}

if (await isDirectCliExecution(import.meta.url, process.argv[1])) {
  const argv = process.argv.slice(2)
  const handled = await maybeHandleAlreadyCurrentUpdate(argv, {
    binaryTarget: Binary.target,
    binaryVersion: Binary.version,
  })
  if (!handled) await cli.serve(argv)
}

function normalizeStableVersion(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const match = value.match(/^[vV]?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/)
  if (!match) return undefined
  return `${match[1]}.${match[2]}.${match[3]}`
}

function resolveUpdateOutputFormat(
  argv: string[],
):
  | { explicit: boolean; format: Formatter.Format; valid: true }
  | { valid: false } {
  const formats = new Set<Formatter.Format>(['toon', 'json', 'yaml', 'md', 'jsonl'])
  let explicit = false
  let format: Formatter.Format = 'toon'

  for (let index = 0; index < argv.length; index++) {
    const token = argv[index]
    if (token === '--json') {
      explicit = true
      format = 'json'
      continue
    }
    if (token !== '--format' || argv[index + 1] === undefined) continue

    const candidate = argv[index + 1] as Formatter.Format
    if (!formats.has(candidate)) return { valid: false }
    explicit = true
    format = candidate
    index++
  }

  return { explicit, format, valid: true }
}

async function loadRuntimeConfig(): Promise<Config> {
  return loadConfig((key) => process.env[key], new KeychainCredentialStore())
}

function canRenderBrowser(formatExplicit: boolean): boolean {
  return !formatExplicit && process.stdout.isTTY === true && process.stdin.isTTY === true
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
  if (error instanceof PolarisError) {
    return c.error({
      code: error.kind.toUpperCase(),
      message: error.message,
      retryable: error.retryable,
      exitCode: error.exitCode(),
    })
  }
  return c.error({
    code: 'OTHER',
    message: error instanceof Error ? error.message : String(error),
    exitCode: 1,
  })
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
