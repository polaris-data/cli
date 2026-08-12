import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { pathToFileURL } from 'node:url'

import {
  cli,
  isDirectCliExecution,
  maybeHandleAlreadyCurrentUpdate,
  resolveCliVersion,
  resolveDefaultMcpCommand,
} from './index.js'
import { basicFixture, MockPolarisServer } from '../../core/test/support/mock-server.js'

async function serve(argv: string[]) {
  let output = ''
  let exitCode: number | undefined
  await cli.serve(argv, {
    stdout(value) {
      output += value
    },
    exit(code) {
      exitCode = code
    },
  })
  return { output, exitCode }
}

async function withEnv<T>(env: Record<string, string>, run: () => Promise<T>): Promise<T> {
  const previous = new Map<string, string | undefined>()
  for (const [key, value] of Object.entries(env)) {
    previous.set(key, process.env[key])
    process.env[key] = value
  }
  try {
    return await run()
  } finally {
    for (const [key, value] of previous) {
      if (value === undefined) delete process.env[key]
      else process.env[key] = value
    }
  }
}

test('catalog --json returns structured output', async () => {
  const fixture = basicFixture()
  const server = new MockPolarisServer(fixture)
  await server.start()
  try {
    const result = await withEnv(
      {
        POLARIS_BASE_URL: server.baseUrl(),
        POLARIS_API_KEY: 'env-key',
      },
      () => serve(['catalog', '--json', '--source', fixture.source, '--market', fixture.market]),
    )
    const parsed = JSON.parse(result.output)
    assert.equal(parsed.command, 'catalog')
    assert.equal(parsed.dataset_total, 1)
    assert.equal(parsed.datasets[0].dataset, 'aster:BTCUSDT')
  } finally {
    await server.close()
  }
})

test('list --json returns local snapshot metadata', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-list-'))
  const filePath = path.join(
    temp,
    'data/standard/aster/BTCUSDT/2026-06-01/standard-aster-BTCUSDT-2026-06-01-00.jsonl.zst',
  )
  await fs.mkdir(path.dirname(filePath), { recursive: true })
  await fs.writeFile(filePath, 'snapshot')

  const result = await withEnv({ POLARIS_ROOT: temp, POLARIS_API_KEY: 'env-key' }, () =>
    serve(['list', '--json', '--source', 'aster']),
  )
  const parsed = JSON.parse(result.output)
  assert.equal(parsed.command, 'list')
  assert.equal(parsed.snapshot_total, 1)
  assert.equal(parsed.snapshots[0].market, 'BTCUSDT')
})

test('download --json reports sync counts', async () => {
  const fixture = basicFixture()
  const server = new MockPolarisServer(fixture)
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-download-'))
  await server.start()
  try {
    const result = await withEnv(
      {
        POLARIS_BASE_URL: server.baseUrl(),
        POLARIS_ROOT: temp,
        POLARIS_API_KEY: 'env-key',
      },
      () =>
        serve([
          'download',
          '--json',
          '--source',
          fixture.source,
          '--market',
          fixture.market,
          '--from',
          fixture.coverage.start,
          '--to',
          fixture.coverage.end,
        ]),
    )
    const parsed = JSON.parse(result.output)
    assert.equal(parsed.command, 'download')
    assert.equal(parsed.downloaded_total, 2)
    assert.equal(parsed.failed_total, 0)
    assert.equal(server.state.batchDownloadCount, 1)
    assert.equal(server.state.keyDownloadCount, 0)
  } finally {
    await server.close()
  }
})

test('reset --json removes local roots', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-reset-'))
  const dataRoot = path.join(temp, 'data')
  await fs.mkdir(path.join(dataRoot, 'sample'), { recursive: true })
  await fs.writeFile(path.join(dataRoot, 'sample', 'file.txt'), 'x')

  const result = await withEnv({ POLARIS_ROOT: temp, POLARIS_API_KEY: 'env-key' }, () =>
    serve(['reset', '--json']),
  )
  const parsed = JSON.parse(result.output)
  assert.equal(parsed.command, 'reset')
  assert.ok(Array.isArray(parsed.removed_roots))
})

test('direct execution detection resolves symlinked entry paths', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-entry-'))
  const realTemp = await fs.realpath(temp)
  const modulePath = path.join(realTemp, 'entry.js')
  const aliasPath = path.join(temp, 'entry.js')

  await fs.writeFile(modulePath, '')

  assert.equal(await isDirectCliExecution(pathToFileURL(modulePath).href, aliasPath), true)
  assert.equal(
    await isDirectCliExecution(pathToFileURL(modulePath).href, path.join(realTemp, 'other.js')),
    false,
  )
})

test('mcp registration prefers the installed polaris binary path', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-bin-'))
  const installDir = path.join(temp, 'bin dir')
  const binaryPath = path.join(installDir, 'polaris')
  await fs.mkdir(installDir, { recursive: true })
  await fs.writeFile(binaryPath, '')
  const resolvedBinaryPath = await fs.realpath(binaryPath)

  assert.equal(
    resolveDefaultMcpCommand(binaryPath, '/usr/local/bin/node'),
    `${JSON.stringify(resolvedBinaryPath)} --mcp`,
  )
})

test('mcp registration falls back to node plus script entry for built sources', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-cli-script-'))
  const entryPath = path.join(temp, 'dist/cli/src/index.js')
  await fs.mkdir(path.dirname(entryPath), { recursive: true })
  await fs.writeFile(entryPath, '')
  const resolvedEntryPath = await fs.realpath(entryPath)

  assert.equal(
    resolveDefaultMcpCommand(entryPath, '/usr/local/bin/node'),
    `/usr/local/bin/node ${resolvedEntryPath} --mcp`,
  )
})

test('mcp registration falls back to bare polaris command when no safe path is available', () => {
  assert.equal(resolveDefaultMcpCommand(undefined, '/usr/local/bin/node'), 'polaris --mcp')
  assert.equal(resolveDefaultMcpCommand('src/index.ts', '/usr/local/bin/node'), 'polaris --mcp')
})

test('CLI version prefers Incur embedded metadata and falls back to package metadata', () => {
  assert.equal(resolveCliVersion('1.2.3', '0.8.3'), '1.2.3')
  assert.equal(resolveCliVersion(undefined, '0.8.3'), '0.8.3')
})

test('--update reports success when the standalone binary is already current', async () => {
  let output = ''
  let requestedUrl = ''
  const handled = await maybeHandleAlreadyCurrentUpdate(['--update'], {
    binaryTarget: 'darwin-arm64',
    binaryVersion: '0.8.4',
    fetchLatest: async (input) => {
      requestedUrl = String(input)
      return Response.json({ tag_name: 'v0.8.4' })
    },
    isTty: true,
    stdout(value) {
      output += value
    },
  })

  assert.equal(handled, true)
  assert.equal(requestedUrl, 'https://api.github.com/repos/polaris-data/cli/releases/latest')
  assert.equal(output, '✓ polaris is already up to date (0.8.4)\n')
})

test('--update formats an already-current result for agents', async () => {
  let output = ''
  const handled = await maybeHandleAlreadyCurrentUpdate(['--update', '--format', 'json'], {
    binaryTarget: 'darwin-arm64',
    binaryVersion: '0.8.4',
    fetchLatest: async () => Response.json({ tag_name: 'v0.8.4' }),
    isTty: true,
    stdout(value) {
      output += value
    },
  })

  assert.equal(handled, true)
  assert.deepEqual(JSON.parse(output), {
    current: '0.8.4',
    name: 'polaris',
    status: 'up_to_date',
  })
})

test('--update delegates to Incur when another release is available', async () => {
  let output = ''
  const handled = await maybeHandleAlreadyCurrentUpdate(['--update'], {
    binaryTarget: 'darwin-arm64',
    binaryVersion: '0.8.4',
    fetchLatest: async () => Response.json({ tag_name: 'v0.8.5' }),
    isTty: true,
    stdout(value) {
      output += value
    },
  })

  assert.equal(handled, false)
  assert.equal(output, '')
})

test('--update delegates to Incur when the latest-release preflight fails', async () => {
  const handled = await maybeHandleAlreadyCurrentUpdate(['--update'], {
    binaryTarget: 'darwin-arm64',
    binaryVersion: '0.8.4',
    fetchLatest: async () => {
      throw new Error('offline')
    },
  })

  assert.equal(handled, false)
})

test('--help takes precedence over the already-current update preflight', async () => {
  let requested = false
  const handled = await maybeHandleAlreadyCurrentUpdate(['--help', '--update'], {
    binaryTarget: 'darwin-arm64',
    binaryVersion: '0.8.4',
    fetchLatest: async () => {
      requested = true
      return Response.json({ tag_name: 'v0.8.4' })
    },
  })

  assert.equal(handled, false)
  assert.equal(requested, false)
})

test('legacy update subcommand is not advertised', async () => {
  const result = await serve(['--help'])
  assert.equal(result.exitCode, undefined)
  assert.doesNotMatch(result.output, /^\s+update\b/m)
})

test('--llms-full includes Polaris docs references in the root skill output', async () => {
  const result = await serve(['--llms-full'])
  assert.match(
    result.output,
    /Before using Polaris commands, read https:\/\/docs\.polaris\.supply\/llms\.txt/,
  )
  assert.match(
    result.output,
    /Start here: read https:\/\/docs\.polaris\.supply\/llms\.txt before using Polaris commands\./,
  )
  assert.match(result.output, /https:\/\/docs\.polaris\.supply/)
  assert.match(result.output, /https:\/\/docs\.polaris\.supply\/sdks\/python/)
  assert.match(result.output, /https:\/\/docs\.polaris\.supply\/sdks\/typescript/)
  assert.match(result.output, /https:\/\/www\.polaris\.supply\/llms\.txt/)
})
