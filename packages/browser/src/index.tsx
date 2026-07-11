import React, { useState, useCallback } from 'react'
import { Box, Text, render, useApp, useInput, useWindowSize } from 'ink'
import TextInput from 'ink-text-input'

import {
  KeychainCredentialStore,
  Layout,
  PolarisClient,
  buildSyncPlan,
  loadAccountIdentity,
  loadBookmarks,
  loadConfig,
  openUrl,
  presentTotal,
  saveAccountIdentity,
  saveBookmarks,
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

export async function runPolarisBrowser(
  client: PolarisClient,
  seed: BrowserSeed = {},
): Promise<number> {
  const store = new KeychainCredentialStore()
  const config = await loadConfig((key) => process.env[key], store)
  const layout = new Layout(config.root)
  const bookmarks = await loadBookmarks(config.root)
  const account = await loadAccountIdentity(config.root)
  const datasets = await loadDatasets(client, seed, '')
  const localSnapshots = await layout.listLocalSnapshots()

  const { waitUntilExit } = render(
    <PolarisBrowser
      client={client}
      config={config}
      layout={layout}
      seed={seed}
      initialBookmarks={bookmarks}
      initialAccount={account}
      initialDatasets={datasets}
      initialLocalSnapshots={localSnapshots}
    />,
  )

  await waitUntilExit()
  return 0
}

interface PolarisBrowserProps {
  client: PolarisClient
  config: any
  layout: Layout
  seed: BrowserSeed
  initialBookmarks: Set<string>
  initialAccount: any
  initialDatasets: BrowserDataset[]
  initialLocalSnapshots: any[]
}

function PolarisBrowser({
  client,
  config,
  layout,
  seed,
  initialBookmarks,
  initialAccount,
  initialDatasets,
  initialLocalSnapshots,
}: PolarisBrowserProps) {
  const { exit } = useApp()
  const { rows, columns } = useWindowSize()

  const [search, setSearch] = useState(seed.search ?? '')
  const [searchInput, setSearchInput] = useState(seed.search ?? '')
  const [status, setStatus] = useState('Ready. Press / to search, q to quit.')
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [bookmarks, setBookmarks] = useState(initialBookmarks)
  const [account, setAccount] = useState(initialAccount)
  const [datasets, setDatasets] = useState(initialDatasets)
  const [localSnapshots, setLocalSnapshots] = useState(initialLocalSnapshots)
  const [isSearching, setIsSearching] = useState(false)

  const refresh = useCallback(async () => {
    const updatedDatasets = await loadDatasets(client, seed, search)
    const updatedLocalSnapshots = await layout.listLocalSnapshots()
    setDatasets(updatedDatasets)
    setLocalSnapshots(updatedLocalSnapshots)
    setSelectedIndex((prev) => Math.min(prev, Math.max(updatedDatasets.length - 1, 0)))
    setStatus(`Loaded ${updatedDatasets.length} dataset(s).`)
  }, [client, seed, search, layout])

  const toggleBookmark = useCallback(async () => {
    const selected = datasets[selectedIndex]
    if (!selected) return

    const updatedBookmarks = new Set(bookmarks)
    if (updatedBookmarks.has(selected.dataset)) {
      updatedBookmarks.delete(selected.dataset)
    } else {
      updatedBookmarks.add(selected.dataset)
    }

    setBookmarks(updatedBookmarks)
    await saveBookmarks(config.root, updatedBookmarks)
    setStatus(updatedBookmarks.has(selected.dataset)
      ? `Bookmarked ${selected.dataset}.`
      : `Removed bookmark for ${selected.dataset}.`)
  }, [bookmarks, datasets, selectedIndex, config.root])

  const startLogin = useCallback(async () => {
    const start = await new PolarisClient(config.baseUrl, undefined, config.timeoutMs).startCliAuth()
    setStatus(`Open browser login for code ${start.user_code}.`)
    await openUrl(start.login_url)

    const pollClient = new PolarisClient(config.baseUrl, undefined, config.timeoutMs)
    let poll = await pollClient.pollCliAuth(start.request_id, start.poll_token)

    while (poll.status === 'pending') {
      setStatus(`Waiting for login approval: ${start.user_code}`)
      await new Promise(resolve => setTimeout(resolve, 250))
      poll = await pollClient.pollCliAuth(start.request_id, start.poll_token)
    }

    if (poll.status === 'approved') {
      const store = new KeychainCredentialStore()
      await store.setApiKey(poll.api_key)
      const updatedConfig = await loadConfig((key) => process.env[key], store)
      const accountClient = new PolarisClient(
        updatedConfig.baseUrl,
        updatedConfig.apiKey,
        updatedConfig.timeoutMs,
      )
      const fetchedAccount = await accountClient.fetchAccount()
      setAccount(fetchedAccount.identity)
      await saveAccountIdentity(config.root, fetchedAccount.identity)
      setStatus(`Signed in as ${fetchedAccount.identity.display_name || fetchedAccount.identity.email || fetchedAccount.user_id}.`)
    } else {
      setStatus(poll.status === 'expired' ? 'Login session expired.' : 'Login session consumed.')
    }
  }, [config, client])

  const previewDownload = useCallback(async () => {
    const selected = datasets[selectedIndex]
    if (!selected) return

    const plan = await buildSyncPlan(client, config, selected.source, selected.market, {
      from: selected.start,
      to: selected.end,
    })
    setStatus(`Plan ready: ${presentTotal(plan)} existing, ${plan.snapshots.length} remote.`)
  }, [datasets, selectedIndex, client, config])

  const viewSnapshots = useCallback(() => {
    const selected = datasets[selectedIndex]
    if (!selected) return

    const matchingLocal = localSnapshots.filter(
      (entry) => entry.source === selected.source && entry.market === selected.market,
    )
    setStatus(`Snapshots for ${selected.dataset}: ${matchingLocal.length} local files found.`)
  }, [datasets, selectedIndex, localSnapshots])

  useInput((input, key) => {
    if (isSearching) {
      if (key.return) {
        setIsSearching(false)
        setSearch(searchInput)
        refresh()
      } else if (key.escape) {
        setIsSearching(false)
        setSearchInput('')
        setSearch('')
      }
      return
    }

    if (input === 'q') {
      exit()
    } else if (key.return) {
      viewSnapshots()
    } else if (key.escape) {
      if (search) {
        setSearch('')
        setSearchInput('')
        refresh()
      }
    } else if (input === 'r') {
      refresh()
    } else if (input === 'b') {
      toggleBookmark()
    } else if (input === 'l') {
      startLogin()
    } else if (input === 'd') {
      previewDownload()
    } else if (input === '/') {
      setIsSearching(true)
    } else if (key.upArrow) {
      setSelectedIndex((prev) => Math.max(0, prev - 1))
    } else if (key.downArrow) {
      setSelectedIndex((prev) => Math.min(datasets.length - 1, prev + 1))
    }
  })

  const selected = datasets[selectedIndex]

  const headerLines = 4
  const listWidth = Math.max(24, Math.min(52, Math.floor(columns * 0.5)))
  const listHeight = Math.max(rows - headerLines, 3)
  const viewportStart = Math.min(
    Math.max(selectedIndex - Math.floor(listHeight / 2), 0),
    Math.max(datasets.length - listHeight, 0),
  )
  const viewportEnd = Math.min(viewportStart + listHeight, datasets.length)
  const hasMoreAbove = viewportStart > 0
  const hasMoreBelow = viewportEnd < datasets.length

  const detailsLines = [
    `status: ${status}`,
    `root: ${config.root}`,
    `auth: ${config.apiKey ? 'configured' : 'not configured'}`,
    `bookmarks: ${bookmarks.size}`,
  ]

  if (account) {
    detailsLines.push(`identity: ${account.display_name || account.email || account.wallet_address || 'saved'}`)
  }

  if (selected) {
    const matchingLocal = localSnapshots.filter(
      (entry) => entry.source === selected.source && entry.market === selected.market,
    )
    detailsLines.push(`selection: ${selected.dataset}`)
    detailsLines.push(`coverage: ${selected.start} -> ${selected.end}`)
    detailsLines.push(`access: ${selected.access || 'public'}`)
    detailsLines.push(`categories: ${selected.categories.length > 0 ? selected.categories.join(', ') : '(none)'}`)
    detailsLines.push(`local snapshots: ${matchingLocal.length}`)
    detailsLines.push(`bookmarked: ${bookmarks.has(selected.dataset) ? 'yes' : 'no'}`)
  } else {
    detailsLines.push('selection: none')
  }

  return (
    <Box flexDirection="column">
      <Box flexDirection="column" marginBottom={1}>
        <Text bold>Polaris browser</Text>
        <Text>
          {isSearching ? (
            <>Search: <TextInput value={searchInput} onChange={setSearchInput} placeholder="Type search term..." /></>
          ) : (
            <>Search: {search || '(none)'}</>
          )}
        </Text>
        <Text dimColor>
          Keys: ↑↓ navigate, Enter select, / search, Esc clear, q quit, r refresh, b bookmark, l login, d plan download
        </Text>
      </Box>

      <Box flexDirection="row">
        <Box flexDirection="column" marginRight={1} width={listWidth}>
          {hasMoreAbove && (
            <Text dimColor>↑ {viewportStart} more</Text>
          )}
          {datasets.slice(viewportStart, viewportEnd).map((dataset, i) => {
            const index = viewportStart + i
            const isSelected = index === selectedIndex
            return (
              <Text
                key={dataset.dataset}
                dimColor={!isSelected}
                bold={isSelected}
                wrap="truncate-end"
              >
                {isSelected ? '▶ ' : '  '}{bookmarks.has(dataset.dataset) ? '*' : ' '} {dataset.dataset}
              </Text>
            )
          })}
          {hasMoreBelow && (
            <Text dimColor>↓ {datasets.length - viewportEnd} more</Text>
          )}
        </Box>

        <Box flexDirection="column">
          {detailsLines.map((line, index) => (
            <Text key={index}>{line}</Text>
          ))}
        </Box>
      </Box>
    </Box>
  )
}

async function loadDatasets(
  client: PolarisClient,
  seed: BrowserSeed,
  search: string,
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
    .filter((entry) => {
      if (!search) return true
      const haystack = [
        entry.dataset,
        entry.source,
        entry.market,
        entry.access ?? '',
        ...entry.categories,
      ]
      .join(' ')
      .toLowerCase()
      return haystack.includes(search.toLowerCase())
    })
    .sort((left, right) => left.dataset.localeCompare(right.dataset))
}
