import { invalidArgument } from './errors.js';

export function parseRfc3339(value: string, flag: string): Date {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    throw invalidArgument(`failed to parse ${flag} as RFC 3339`);
  }
  return parsed;
}
