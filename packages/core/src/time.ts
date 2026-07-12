export function toRfc3339(date: Date): string {
  return new Date(Math.trunc(date.getTime() / 1000) * 1000).toISOString().replace('.000', '')
}

export function parseDateOnly(value: string): Date | undefined {
  const parsed = new Date(`${value}T00:00:00Z`)
  return Number.isNaN(parsed.getTime()) ? undefined : parsed
}

export function parseRfc3339(value: string, flag: string): Date {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    throw new Error(`failed to parse ${flag} as RFC 3339`)
  }
  return parsed
}
