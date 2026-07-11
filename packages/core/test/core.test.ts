import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

import {
  Layout,
  PolarisClient,
  acquireSyncLock,
  buildSyncPlan,
  classifySnapshots,
  executeSync,
  loadConfig,
  selectDefaultRoot,
} from '../src/index.js'
import { basicFixture, MockPolarisServer } from './support/mock-server.js'

test('env API key overrides stored key', async () => {
  const config = await loadConfig(
    (key) => (key === 'POLARIS_API_KEY' ? 'env-key' : undefined),
    {
      async getApiKey() {
        return 'stored-key'
      },
      async setApiKey() {},
    },
  )
  assert.equal(config.apiKey, 'env-key')
  assert.equal(config.apiKeySource, 'environment')
})

test('selectDefaultRoot prefers existing legacy root until new root exists', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-core-'))
  const primary = path.join(temp, 'polaris')
  const legacy = path.join(temp, 'tick')
  await fs.mkdir(legacy)
  assert.equal(selectDefaultRoot(primary, legacy), legacy)
  await fs.mkdir(primary)
  assert.equal(selectDefaultRoot(primary, legacy), primary)
})

test('layout maps opaque keys to canonical paths', async () => {
  const layout = new Layout('/tmp/polaris')
  assert.equal(
    layout.dataPathForKey('standard-aster-BTCUSDT-2026-06-01-00'),
    '/tmp/polaris/data/standard/aster/BTCUSDT/2026-06-01/standard-aster-BTCUSDT-2026-06-01-00.jsonl.zst',
  )
})

test('layout preserves dashes inside market names', async () => {
  const layout = new Layout('/tmp/polaris')
  assert.equal(
    layout.dataPathForKey('standard-arcus-AAPL-USD-2026-07-11-00'),
    '/tmp/polaris/data/standard/arcus/AAPL-USD/2026-07-11/standard-arcus-AAPL-USD-2026-07-11-00.jsonl.zst',
  )
})

test('sync lock can be reacquired after release', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-lock-'))
  const layout = new Layout(temp)
  const first = await acquireSyncLock(layout)
  await assert.rejects(() => acquireSyncLock(layout))
  await first.release()
  const second = await acquireSyncLock(layout)
  await second.release()
})

test('catalog and snapshot pagination drive the sync plan', async () => {
  const fixture = basicFixture()
  const server = new MockPolarisServer(fixture)
  await server.start()
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-sync-'))
  try {
    const client = new PolarisClient(server.baseUrl(), undefined, 5_000)
    const config = {
      baseUrl: server.baseUrl(),
      root: temp,
      concurrency: 2,
      timeoutMs: 5_000,
    }
    const plan = await buildSyncPlan(client, config, fixture.source, fixture.market, {
      from: fixture.coverage.start,
      to: fixture.coverage.end,
    })
    assert.equal(plan.snapshots.length, 2)

    const execution = await executeSync(client, plan, 2)
    assert.equal(execution.downloadedKeys.length, 2)
    assert.equal(server.state.batchDownloadCount, 1)
    assert.equal(server.state.keyDownloadCount, 0)
    const layout = new Layout(temp)
    const bytes = await fs.readFile(layout.dataPathForKey(fixture.pages[0]![0]!.key), 'utf8')
    assert.equal(bytes, 'snapshot-0')
  } finally {
    await server.close()
  }
})

test('incomplete temp files classify as incomplete', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'polaris-classify-'))
  const layout = new Layout(temp)
  await fs.mkdir(path.dirname(layout.tempPathForKey('standard-source-market-2026-01-01-incomplete')), {
    recursive: true,
  })
  await fs.writeFile(layout.tempPathForKey('standard-source-market-2026-01-01-incomplete'), 'partial')
  const snapshots = await classifySnapshots(layout, [
    { key: 'standard-source-market-2026-01-01-incomplete' },
  ])
  assert.equal(snapshots[0]?.state, 'incomplete')
})
