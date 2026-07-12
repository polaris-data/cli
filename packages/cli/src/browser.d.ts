export interface BrowserSeed {
  source?: string
  market?: string
  search?: string
}

export declare function runPolarisBrowser(
  client: any,
  seed?: BrowserSeed
): Promise<number>
