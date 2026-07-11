import React, { useState, useEffect, useMemo, useCallback } from 'react'
import { Box, Text, render, useApp, useInput, useWindowSize } from 'ink'
import fs from 'node:fs/promises'
import path from 'node:path'

import {
  KeychainCredentialStore,
  Layout,
  PolarisClient,
  PolarisError,
  acquireSyncLock,
  buildSyncPlan,
  executeSync,
  inferDateFromText,
  loadAccountIdentity,
  loadConfig,
  openUrl,
  parseOpaqueKey,
  type Config,
  type SyncPlan,
  type SnapshotPlan,
} from '@polaris/core'

export interface BrowserSeed {
  source?: string
  market?: string
  search?: string
}

interface BrowserDataset {
  source: string
  market: string
  start: string
  end: string
  dataset: string
  categories: string[]
  access?: string | undefined
}

interface DateGroup {
  date: string
  total: number
  present: number
  missing: number
  incomplete: number
}

export async function runPolarisBrowser(
  client: PolarisClient,
  seed: BrowserSeed = {},
): Promise<number> {
  const store = new KeychainCredentialStore()
  const config = await loadConfig((key) => process.env[key], store)
  const layout = new Layout(config.root)
  const account = await loadAccountIdentity(config.root)
  const markets = await loadDatasets(client, seed)

  const { waitUntilExit } = render(
    <PolarisBrowser
      client={client}
      config={config}
      layout={layout}
      seed={seed}
      initialAccount={account}
      initialMarkets={markets}
    />,
  )

  await waitUntilExit()
  return 0
}

interface PolarisBrowserProps {
  client: PolarisClient
  config: Config
  layout: Layout
  seed: BrowserSeed
  initialAccount: any
  initialMarkets: BrowserDataset[]
}

function PolarisBrowser({
  client,
  config,
  layout,
  seed,
  initialAccount,
  initialMarkets,
}: PolarisBrowserProps) {
  const { exit } = useApp()
  const { rows, columns } = useWindowSize()

  const [search, setSearch] = useState(seed.search ?? '')
  const [focus, setFocus] = useState<'markets' | 'dates'>('markets')
  const [selectedMarketIndex, setSelectedMarketIndex] = useState(0)
  const [selectedDateIndex, setSelectedDateIndex] = useState(0)
  const [plan, setPlan] = useState<SyncPlan | null>(null)
  const [loadingDates, setLoadingDates] = useState(false)
  const [status, setStatus] = useState('Type to search, Esc to quit.')
  const [downloading, setDownloading] = useState(false)
  const [progress, setProgress] = useState({ done: 0, total: 0, failed: 0 })

  const filteredMarkets = useMemo(
    () => filterMarkets(initialMarkets, search),
    [initialMarkets, search],
  )

  const dateGroups = useMemo(
    () => (plan ? groupByDate(plan.snapshots) : []),
    [plan],
  )

  useEffect(() => {
    setSelectedMarketIndex((prev) => Math.min(prev, Math.max(filteredMarkets.length - 1, 0)))
  }, [filteredMarkets])

  useEffect(() => {
    setSelectedDateIndex((prev) => Math.min(prev, Math.max(dateGroups.length - 1, 0)))
  }, [dateGroups])

  const enterMarket = useCallback(async () => {
    const market = filteredMarkets[selectedMarketIndex]
    if (!market) return
    setLoadingDates(true)
    setStatus(`Loading dates for ${market.dataset}...`)
    try {
      const syncPlan = await buildSyncPlan(client, config, market.source, market.market, {
        from: market.start,
        to: market.end,
      })
      setPlan(syncPlan)
      setFocus('dates')
      setSelectedDateIndex(0)
      const groups = groupByDate(syncPlan.snapshots)
      setStatus(`${syncPlan.snapshots.length} snapshots across ${groups.length} dates.`)
    } catch (error) {
      setStatus(`Failed to load: ${error instanceof Error ? error.message : String(error)}`)
    } finally {
      setLoadingDates(false)
    }
  }, [filteredMarkets, selectedMarketIndex, client, config])

  const downloadDate = useCallback(async (dateGroup: DateGroup) => {
    if (!plan || downloading) return
    setDownloading(true)
    setProgress({ done: 0, total: dateGroup.missing, failed: 0 })
    setStatus(`Downloading ${dateGroup.date}...`)
    try {
      let guard
      try {
        guard = await acquireSyncLock(layout)
      } catch (error) {
        if (error instanceof PolarisError && error.kind === 'lock_held' && error.path) {
          await fs.rm(error.path, { force: true })
          guard = await acquireSyncLock(layout)
        } else {
          throw error
        }
      }
      try {
        const datePlan: SyncPlan = {
          ...plan,
          snapshots: plan.snapshots.filter((s) => inferDateFromText(s.key) === dateGroup.date),
        }
        const execution = await executeSync(client, datePlan, config.concurrency, (event) => {
          if (event.type === 'downloaded') {
            setProgress((p) => ({ ...p, done: p.done + 1 }))
          } else if (event.type === 'failed') {
            setProgress((p) => ({ ...p, failed: p.failed + 1, done: p.done + 1 }))
          }
        })
        const downloaded = new Set(execution.downloadedKeys)
        setPlan((prev) =>
          prev
            ? {
                ...prev,
                snapshots: prev.snapshots.map((s) =>
                  downloaded.has(s.key) ? { ...s, state: 'present' as const } : s,
                ),
              }
            : null,
        )
        setStatus(
          `Downloaded ${execution.downloadedKeys.length}, failed ${execution.failed.length} for ${dateGroup.date}.`,
        )
      } finally {
        await guard.release()
      }
    } catch (error) {
      setStatus(`Download failed: ${error instanceof Error ? error.message : String(error)}`)
    } finally {
      setDownloading(false)
    }
  }, [plan, downloading, layout, client, config])

  useInput((input, key) => {
    if (downloading) return

    if (key.escape) {
      exit()
      return
    }

    if (key.backspace || key.delete) {
      if (focus === 'dates') {
        setFocus('markets')
        setPlan(null)
      }
      setSearch((prev) => prev.slice(0, -1))
      return
    }

    if (
      input &&
      !key.ctrl &&
      !key.meta &&
      !key.tab &&
      !key.return &&
      !key.upArrow &&
      !key.downArrow &&
      !key.leftArrow &&
      !key.rightArrow &&
      !key.escape &&
      !key.backspace &&
      !key.delete
    ) {
      if (focus === 'dates') {
        setFocus('markets')
        setPlan(null)
      }
      setSearch((prev) => prev + input)
      return
    }

    if (key.upArrow) {
      if (focus === 'markets') {
        setSelectedMarketIndex((prev) => Math.max(0, prev - 1))
      } else {
        setSelectedDateIndex((prev) => Math.max(0, prev - 1))
      }
      return
    }
    if (key.downArrow) {
      if (focus === 'markets') {
        setSelectedMarketIndex((prev) => Math.min(filteredMarkets.length - 1, prev + 1))
      } else {
        setSelectedDateIndex((prev) => Math.min(dateGroups.length - 1, prev + 1))
      }
      return
    }
    if (key.return) {
      if (focus === 'markets') {
        enterMarket()
      } else {
        const dateGroup = dateGroups[selectedDateIndex]
        if (dateGroup) downloadDate(dateGroup)
      }
      return
    }

    if (key.rightArrow) {
      if (focus === 'markets') {
        enterMarket()
      }
      return
    }
    if (key.leftArrow) {
      if (focus === 'dates') {
        setFocus('markets')
        setPlan(null)
      }
      return
    }
    if (key.tab) {
      if (focus === 'markets' && filteredMarkets[selectedMarketIndex]) {
        const market = filteredMarkets[selectedMarketIndex]
        const dataDir = path.join(config.root, 'data')
        ;(async () => {
          try {
            for (const tier of await fs.readdir(dataDir)) {
              const marketDir = path.join(dataDir, tier, market.source, market.market)
              try {
                await fs.access(marketDir)
                await openUrl(marketDir)
                setStatus(`Opened ${marketDir} in Finder.`)
                return
              } catch {}
            }
          } catch {}
          setStatus(`No local data found for ${market.dataset}.`)
        })()
      } else if (focus === 'dates' && dateGroups[selectedDateIndex] && plan) {
        const dateGroup = dateGroups[selectedDateIndex]
        const snapshot = plan.snapshots.find((s) => inferDateFromText(s.key) === dateGroup.date)
        if (snapshot) {
          const dateDir = path.dirname(layout.dataPathForKey(snapshot.key))
          ;(async () => {
            try {
              await fs.access(dateDir)
              await openUrl(dateDir)
              setStatus(`Opened ${dateDir} in Finder.`)
            } catch {
              setStatus(`No local data found for ${dateGroup.date}.`)
            }
          })()
        }
      }
      return
    }
    if (key.return) {
      if (focus === 'dates' && dateGroups[selectedDateIndex]) {
        downloadDate(dateGroups[selectedDateIndex])
      }
      return
    }
  })

  const headerLines = 2
  const searchLine = 1
  const availableHeight = Math.max(rows - headerLines - searchLine, 3)

  const rootWidth = 10
  const leftWidth = Math.max(20, Math.floor((columns - rootWidth) * 0.28))
  const middleWidth = Math.max(20, Math.floor((columns - rootWidth) * 0.26))

  const marketViewportStart = Math.min(
    Math.max(selectedMarketIndex - Math.floor(availableHeight / 2), 0),
    Math.max(filteredMarkets.length - availableHeight, 0),
  )
  const marketViewportEnd = Math.min(marketViewportStart + availableHeight, filteredMarkets.length)

  const dateViewportStart = Math.min(
    Math.max(selectedDateIndex - Math.floor(availableHeight / 2), 0),
    Math.max(dateGroups.length - availableHeight, 0),
  )
  const dateViewportEnd = Math.min(dateViewportStart + availableHeight, dateGroups.length)

  const selectedMarket = filteredMarkets[selectedMarketIndex]
  const selectedDate = dateGroups[selectedDateIndex]

  const detailsLines: string[] = [
    `status: ${downloading ? `Downloading ${selectedDate?.date ?? ''}: ${progress.done}/${progress.total}` : status}`,
  ]
  if (downloading && progress.total > 0) {
    detailsLines.push(renderProgressBar(progress.done, progress.total, progress.failed))
  }
  detailsLines.push(
    `root: ${config.root}`,
    `auth: ${config.apiKey ? 'configured' : 'not configured'}`,
  )

  if (initialAccount) {
    detailsLines.push(`identity: ${initialAccount.display_name || initialAccount.email || initialAccount.wallet_address || 'saved'}`)
  }

  if (focus === 'markets' && selectedMarket) {
    detailsLines.push('')
    detailsLines.push(`market: ${selectedMarket.dataset}`)
    detailsLines.push(`coverage: ${selectedMarket.start} -> ${selectedMarket.end}`)
    detailsLines.push(`access: ${selectedMarket.access || 'public'}`)
    detailsLines.push(`categories: ${selectedMarket.categories.length > 0 ? selectedMarket.categories.join(', ') : '(none)'}`)
    detailsLines.push('')
    detailsLines.push('→ enter to browse dates')
  } else if (focus === 'dates' && selectedDate) {
    detailsLines.push('')
    detailsLines.push(`date: ${selectedDate.date}`)
    detailsLines.push(`snapshots: ${selectedDate.total}`)
    detailsLines.push(`present: ${selectedDate.present}`)
    detailsLines.push(`missing: ${selectedDate.missing}`)
    if (selectedDate.incomplete > 0) detailsLines.push(`incomplete: ${selectedDate.incomplete}`)
    detailsLines.push('')
    if (selectedDate.missing > 0) {
      detailsLines.push(downloading ? 'downloading...' : 'Enter to download')
    } else {
      detailsLines.push('all snapshots present')
    }
  }

  return (
    <Box flexDirection="column">
      <Box flexDirection="column" marginBottom={1}>
        <Text bold>Polaris browser</Text>
        <Text dimColor>↑↓ navigate · → enter market · ← back · type to filter · Enter download · Tab show in Finder · Esc quit</Text>
      </Box>

      <Box flexDirection="row">
        {focus === 'markets' ? (
          <Box flexDirection="column" width={rootWidth} marginRight={1}>
            <Text bold dimColor>Polaris</Text>
          </Box>
        ) : (
          <Box flexDirection="column" width={leftWidth} marginRight={1}>
            <Text dimColor={!search}>
              search: {search || 'type to filter...'}
            </Text>
            {marketViewportStart > 0 && (
              <Text dimColor>↑ {marketViewportStart} more</Text>
            )}
            {filteredMarkets.slice(marketViewportStart, marketViewportEnd).map((market, i) => {
              const index = marketViewportStart + i
              const isSelected = false
              return (
                <Text
                  key={market.dataset}
                  dimColor={!isSelected}
                  bold={isSelected}
                  wrap="truncate-end"
                >
                  {'  '}{market.dataset}
                </Text>
              )
            })}
            {marketViewportEnd < filteredMarkets.length && (
              <Text dimColor>↓ {filteredMarkets.length - marketViewportEnd} more</Text>
            )}
          </Box>
        )}

        <Box flexDirection="column" width={focus === 'markets' ? leftWidth : middleWidth} marginRight={1}>
          {focus === 'markets' ? (
            <>
              <Text dimColor={!search}>
                search: {search || 'type to filter...'}
              </Text>
              {marketViewportStart > 0 && (
                <Text dimColor>↑ {marketViewportStart} more</Text>
              )}
              {filteredMarkets.slice(marketViewportStart, marketViewportEnd).map((market, i) => {
                const index = marketViewportStart + i
                const isSelected = index === selectedMarketIndex
                return (
                  <Text
                    key={market.dataset}
                    dimColor={!isSelected}
                    bold={isSelected}
                    wrap="truncate-end"
                  >
                    {isSelected ? '▶ ' : '  '}{market.dataset}
                  </Text>
                )
              })}
              {marketViewportEnd < filteredMarkets.length && (
                <Text dimColor>↓ {filteredMarkets.length - marketViewportEnd} more</Text>
              )}
            </>
          ) : loadingDates ? (
            <Text dimColor>Loading dates...</Text>
          ) : dateGroups.length > 0 ? (
            <>
              {dateViewportStart > 0 && (
                <Text dimColor>↑ {dateViewportStart} more</Text>
              )}
              {dateGroups.slice(dateViewportStart, dateViewportEnd).map((dg, i) => {
                const index = dateViewportStart + i
                const isSelected = index === selectedDateIndex
                return (
                  <Text
                    key={dg.date}
                    dimColor={!isSelected}
                    bold={isSelected}
                    wrap="truncate-end"
                  >
                    {isSelected ? '▶ ' : '  '}{dg.date} ({dg.present}/{dg.total})
                  </Text>
                )
              })}
              {dateViewportEnd < dateGroups.length && (
                <Text dimColor>↓ {dateGroups.length - dateViewportEnd} more</Text>
              )}
            </>
          ) : (
            <Text dimColor>No dates available</Text>
          )}
        </Box>

        <Box flexDirection="column">
          {detailsLines.map((line, index) => (
            <Text key={index} wrap="truncate-end">{line}</Text>
          ))}
        </Box>
      </Box>
    </Box>
  )
}

function filterMarkets(markets: BrowserDataset[], search: string): BrowserDataset[] {
  if (!search) return markets
  const normalized = search.toLowerCase()
  return markets.filter((entry) => {
    const haystack = [
      entry.dataset,
      entry.source,
      entry.market,
      entry.access ?? '',
      ...entry.categories,
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(normalized)
  })
}

function groupByDate(snapshots: SnapshotPlan[]): DateGroup[] {
  const map = new Map<string, DateGroup>()
  for (const s of snapshots) {
    const date = inferDateFromText(s.key) ?? 'unknown'
    let group = map.get(date)
    if (!group) {
      group = { date, total: 0, present: 0, missing: 0, incomplete: 0 }
      map.set(date, group)
    }
    group.total++
    if (s.state === 'present') group.present++
    else if (s.state === 'missing') group.missing++
    else if (s.state === 'incomplete') group.incomplete++
  }
  return [...map.values()].sort((a, b) => b.date.localeCompare(a.date))
}

async function loadDatasets(
  client: PolarisClient,
  seed: BrowserSeed,
): Promise<BrowserDataset[]> {
  const catalog = await client.fetchCatalog(seed.source, seed.market)
  return catalog.markets
    .map((market) => ({
      source: market.source,
      market: market.market,
      start: market.start,
      end: market.end,
      dataset: `${market.source}:${market.market}`,
      categories: market.categories,
      access: market.access
        ? market.access.status === 'preview' && market.access.public_cutoff_date
          ? `preview from ${market.access.public_cutoff_date}`
          : market.access.status
        : undefined,
    }))
    .sort((left, right) => left.dataset.localeCompare(right.dataset))
}

function renderProgressBar(done: number, total: number, failed: number): string {
  const barWidth = 20
  const filled = total > 0 ? Math.round((done / total) * barWidth) : 0
  const bar = '█'.repeat(filled) + '░'.repeat(Math.max(0, barWidth - filled))
  const pct = total > 0 ? Math.round((done / total) * 100) : 0
  const failedSuffix = failed > 0 ? `, ${failed} failed` : ''
  return `${bar} ${pct}% (${done}/${total}${failedSuffix})`
}
