import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { pathToFileURL } from 'node:url'

import { cli, isDirectCliExecution } from './index.js'
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
