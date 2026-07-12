import React, { useState, useEffect, useMemo, useCallback, useRef } from 'react'
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

interface LoadedMarketDates {
  plan: SyncPlan
  dateGroups: DateGroup[]
}

const POLARIS_ICON_ASCII = [
  '   ███████',
  '▄▄▄███████',
  '██████████',
  '███████▀▀▀',
  '███████',
] as const

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
  const [showSearchInput, setShowSearchInput] = useState(true)
  const [plan, setPlan] = useState<SyncPlan | null>(null)
  const [datasetCache, setDatasetCache] = useState<Record<string, LoadedMarketDates>>({})
  const [loadingDates, setLoadingDates] = useState(false)
  const [previewLoadingDataset, setPreviewLoadingDataset] = useState<string | null>(null)
  const [status, setStatus] = useState('Type to search, Esc to quit.')
  const [downloading, setDownloading] = useState(false)
  const [activeDownloadDate, setActiveDownloadDate] = useState<string | null>(null)
  const [progress, setProgress] = useState({ done: 0, total: 0, failed: 0 })
  const datasetCacheRef = useRef<Record<string, LoadedMarketDates>>({})
  const inFlightLoadsRef = useRef<Record<string, Promise<LoadedMarketDates>>>({})
  const previewRequestIdRef = useRef(0)
  const activePlanDatasetRef = useRef<string | null>(null)

  const filteredMarkets = useMemo(
    () => filterMarkets(initialMarkets, search),
    [initialMarkets, search],
  )

  const selectedMarket = filteredMarkets[selectedMarketIndex]
  const activePlanDataset = plan ? `${plan.source}:${plan.market}` : null

  const dateGroups = useMemo(
    () => (plan ? groupByDate(plan.snapshots) : []),
    [plan],
  )

  const previewEntry = selectedMarket ? datasetCache[selectedMarket.dataset] : undefined
  const previewDateGroups = previewEntry?.dateGroups ?? []

  useEffect(() => {
    setSelectedMarketIndex((prev) => Math.min(prev, Math.max(filteredMarkets.length - 1, 0)))
  }, [filteredMarkets])

  useEffect(() => {
    setSelectedDateIndex((prev) => Math.min(prev, Math.max(dateGroups.length - 1, 0)))
  }, [dateGroups])

  useEffect(() => {
    if (activePlanDataset && activePlanDataset !== activePlanDatasetRef.current) {
      setSelectedDateIndex(0)
    }
    activePlanDatasetRef.current = activePlanDataset
  }, [activePlanDataset])

  const cacheMarketDates = useCallback((datasetKey: string, loaded: LoadedMarketDates) => {
    setDatasetCache((prev) => {
      const next = { ...prev, [datasetKey]: loaded }
      datasetCacheRef.current = next
      return next
    })
  }, [])

  const loadMarketDates = useCallback(async (market: BrowserDataset): Promise<LoadedMarketDates> => {
    const cached = datasetCacheRef.current[market.dataset]
    if (cached) return cached

    const existingRequest = inFlightLoadsRef.current[market.dataset]
    if (existingRequest) return existingRequest

    const request = (async () => {
      const nextPlan = await buildSyncPlan(client, config, market.source, market.market, {
        from: market.start,
        to: market.end,
      })
      const loaded = {
        plan: nextPlan,
        dateGroups: groupByDate(nextPlan.snapshots),
      }
      cacheMarketDates(market.dataset, loaded)
      return loaded
    })()

    inFlightLoadsRef.current[market.dataset] = request

    try {
      return await request
    } finally {
      delete inFlightLoadsRef.current[market.dataset]
    }
  }, [client, config, cacheMarketDates])

  const syncCachedPlan = useCallback((nextPlan: SyncPlan) => {
    cacheMarketDates(`${nextPlan.source}:${nextPlan.market}`, {
      plan: nextPlan,
      dateGroups: groupByDate(nextPlan.snapshots),
    })
  }, [cacheMarketDates])

  const enterMarket = useCallback(async () => {
    const market = filteredMarkets[selectedMarketIndex]
    if (!market) return
    setLoadingDates(true)
    setStatus(`Loading dates for ${market.dataset}...`)
    try {
      const loaded = await loadMarketDates(market)
      setPlan(loaded.plan)
      setFocus('dates')
      setStatus(`${loaded.plan.snapshots.length} snapshots across ${loaded.dateGroups.length} dates.`)
    } catch (error) {
      setStatus(`Failed to load: ${error instanceof Error ? error.message : String(error)}`)
    } finally {
      setLoadingDates(false)
    }
  }, [filteredMarkets, selectedMarketIndex, loadMarketDates])

  useEffect(() => {
    const requestId = previewRequestIdRef.current + 1
    previewRequestIdRef.current = requestId

    if (focus !== 'markets') {
      setPreviewLoadingDataset(null)
      return
    }

    if (!selectedMarket) {
      setPreviewLoadingDataset(null)
      return
    }

    if (datasetCacheRef.current[selectedMarket.dataset]) {
      setPreviewLoadingDataset(null)
      return
    }

    setPreviewLoadingDataset(selectedMarket.dataset)
    const timeout = setTimeout(() => {
      void (async () => {
        try {
          await loadMarketDates(selectedMarket)
        } catch (error) {
          if (previewRequestIdRef.current === requestId) {
            setStatus(`Failed to load: ${error instanceof Error ? error.message : String(error)}`)
          }
        } finally {
          if (previewRequestIdRef.current === requestId) {
            setPreviewLoadingDataset(null)
          }
        }
      })()
    }, 150)

    return () => {
      clearTimeout(timeout)
    }
  }, [focus, selectedMarket, loadMarketDates])

  const downloadDate = useCallback(async (dateGroup: DateGroup) => {
    if (!plan || downloading) return
    setDownloading(true)
    setActiveDownloadDate(dateGroup.date)
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
        const nextPlan: SyncPlan = {
          ...plan,
          snapshots: plan.snapshots.map((s) =>
            downloaded.has(s.key) ? { ...s, state: 'present' as const } : s,
          ),
        }
        setPlan(nextPlan)
        syncCachedPlan(nextPlan)
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
      setActiveDownloadDate(null)
    }
  }, [plan, downloading, layout, client, config, syncCachedPlan])

  useInput((input, key) => {
    if (downloading) return

    if (key.escape) {
      exit()
      return
    }

    if (key.backspace || key.delete) {
      setShowSearchInput(true)
      if (focus === 'dates') {
        setFocus('markets')
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
      setShowSearchInput(true)
      if (focus === 'dates') {
        setFocus('markets')
      }
      setSearch((prev) => prev + input)
      return
    }

    if (key.upArrow) {
      setShowSearchInput(false)
      if (focus === 'markets') {
        setSelectedMarketIndex((prev) => Math.max(0, prev - 1))
      } else {
        setSelectedDateIndex((prev) => Math.max(0, prev - 1))
      }
      return
    }
    if (key.downArrow) {
      setShowSearchInput(false)
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
  })

  const headerLines = 0
  const searchLine = 1
  const availableHeight = Math.max(rows - headerLines - searchLine, 3)
  const marketListHeight = Math.max(availableHeight - 1, 1)
  const columnGap = 8

  const marketFocusWidths = allocateColumnWidths(columns, [0.5, 0.3, 0.2], [18, 20, 16], columnGap)
  const dateFocusWidths = allocateColumnWidths(columns, [0.18, 0.26, 0.50, 0.06], [12, 18, 22, 2], columnGap)

  const marketViewportStart = Math.min(
    Math.max(selectedMarketIndex - Math.floor(marketListHeight / 2), 0),
    Math.max(filteredMarkets.length - marketListHeight, 0),
  )
  const marketViewportEnd = Math.min(marketViewportStart + marketListHeight, filteredMarkets.length)

  const dateViewportStart = Math.min(
    Math.max(selectedDateIndex - Math.floor(availableHeight / 2), 0),
    Math.max(dateGroups.length - availableHeight, 0),
  )
  const dateViewportEnd = Math.min(dateViewportStart + availableHeight, dateGroups.length)
  const previewDateViewportStart = 0
  const previewDateViewportEnd = Math.min(previewDateViewportStart + availableHeight, previewDateGroups.length)
  const marketListHeader = !showSearchInput && marketViewportStart > 0
    ? `↑ ${marketViewportStart} more`
    : `search: ${search || 'type to filter...'}`

  return (
    <Box flexDirection="column">
      {focus === 'markets' ? (
        <Box flexDirection="row">
          <Box flexDirection="column" width={marketFocusWidths[0]} marginRight={columnGap}>
            <Box flexDirection="column" marginLeft={2}>
              {POLARIS_ICON_ASCII.map((line, index) => (
                <Text key={index} bold wrap="truncate-end">{line}</Text>
              ))}
              <Text> </Text>
              <Text bold>Polaris - Frontier Market Data</Text>
              <Text> </Text>
              <Text dimColor wrap="wrap">↑↓ navigate · → enter market</Text>
              <Text dimColor wrap="wrap">← back · type to filter</Text>
              <Text dimColor wrap="wrap">Enter download · Esc quit</Text>
            </Box>
          </Box>

          <Box flexDirection="column" width={marketFocusWidths[1]} marginRight={columnGap}>
            <Text dimColor={showSearchInput && !search}>{marketListHeader}</Text>
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
          </Box>

          <Box flexDirection="column" width={marketFocusWidths[2]}>
            {!selectedMarket ? (
              <Text dimColor>No markets match the current filter</Text>
            ) : previewLoadingDataset === selectedMarket.dataset ? (
              <Text dimColor>Loading dates...</Text>
            ) : previewEntry ? (
              previewDateGroups.length > 0 ? (
                <>
                  {previewDateViewportStart > 0 && (
                    <Text dimColor>↑ {previewDateViewportStart} more</Text>
                  )}
                  {previewDateGroups.slice(previewDateViewportStart, previewDateViewportEnd).map((dg) => {
                    const completionBar = renderCompletionBar(dg.present, dg.total, 10)
                    const completionCount = formatCompletionCount(dg.present, dg.total)
                    return (
                      <Text
                        key={dg.date}
                        dimColor
                        wrap="truncate-end"
                      >
                        {'  '}{dg.date} {completionCount} {completionBar}
                      </Text>
                    )
                  })}
                  {previewDateViewportEnd < previewDateGroups.length && (
                    <Text dimColor>↓ {previewDateGroups.length - previewDateViewportEnd} more</Text>
                  )}
                </>
              ) : (
                <Text dimColor>No dates available</Text>
              )
            ) : (
              <Text dimColor>Loading dates...</Text>
            )}
          </Box>
        </Box>
      ) : (
        <Box flexDirection="row">
          <Box flexDirection="column" width={dateFocusWidths[0]} marginRight={columnGap}>
            {POLARIS_ICON_ASCII.map((line, index) => (
              <Text key={index} bold wrap="truncate-start">{line}</Text>
            ))}
          </Box>

          <Box flexDirection="column" width={dateFocusWidths[1]} marginRight={columnGap}>
            <Text dimColor={showSearchInput && !search}>{marketListHeader}</Text>
            {filteredMarkets.slice(marketViewportStart, marketViewportEnd).map((market, i) => {
              const index = marketViewportStart + i
              const isSelected = index === selectedMarketIndex
              return (
                <Text
                  key={market.dataset}
                  dimColor
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
          </Box>

          <Box flexDirection="column" width={dateFocusWidths[2]} marginRight={columnGap}>
            {loadingDates ? (
              <Text dimColor>Loading dates...</Text>
            ) : dateGroups.length > 0 ? (
              <>
                {dateViewportStart > 0 && (
                  <Text dimColor>↑ {dateViewportStart} more</Text>
                )}
                {dateGroups.slice(dateViewportStart, dateViewportEnd).map((dg, i) => {
                  const index = dateViewportStart + i
                  const isSelected = index === selectedDateIndex
                  const downloadedNow = downloading && dg.date === activeDownloadDate
                    ? progress.done - progress.failed
                    : 0
                  const livePresent = Math.min(dg.total, dg.present + downloadedNow)
                  const completionBar = renderCompletionBar(livePresent, dg.total, 10)
                  const completionCount = formatCompletionCount(livePresent, dg.total)
                  return (
                    <Text
                      key={dg.date}
                      dimColor={!isSelected}
                      bold={isSelected}
                      wrap="truncate-end"
                    >
                      {isSelected ? '▶ ' : '  '}{dg.date} {completionCount} {completionBar}
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

          <Box flexDirection="column" width={dateFocusWidths[3]}>
            <Text wrap="truncate-end">{' '}</Text>
          </Box>
        </Box>
      )}
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

function renderCompletionBar(present: number, total: number, barWidth: number): string {
  if (total === 0) return '░'.repeat(barWidth)
  const filled = Math.round((present / total) * barWidth)
  return '█'.repeat(filled) + '░'.repeat(Math.max(0, barWidth - filled))
}

function formatCompletionCount(present: number, total: number): string {
  return `${String(present).padStart(3, ' ')}/${String(total).padStart(3, ' ')}`
}

function allocateColumnWidths(
  totalWidth: number,
  ratios: number[],
  minimums: number[],
  gapSize: number,
): number[] {
  const gapWidth = (ratios.length - 1) * gapSize
  const usableWidth = Math.max(totalWidth - gapWidth, ratios.length)
  let widths = ratios.map((ratio, index) => Math.max(minimums[index] ?? 1, Math.floor(usableWidth * ratio)))

  const widthWithoutGaps = widths.reduce((sum, width) => sum + width, 0)
  if (widthWithoutGaps > usableWidth) {
    widths = ratios.map((ratio) => Math.max(1, Math.floor(usableWidth * ratio)))
  }

  const consumed = widths.slice(0, -1).reduce((sum, width) => sum + width, 0)
  widths[widths.length - 1] = Math.max(1, usableWidth - consumed)
  return widths
}
