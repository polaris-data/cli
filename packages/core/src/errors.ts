export class PolarisError extends Error {
  readonly kind:
    | 'dataset_unavailable'
    | 'invalid_argument'
    | 'lock_held'
    | 'request'
    | 'other'
  readonly status?: number
  readonly retryable: boolean
  readonly path?: string
  cause?: unknown

  constructor(
    kind: PolarisError['kind'],
    message: string,
    options: {
      status?: number
      retryable?: boolean
      path?: string
      cause?: unknown
    } = {},
  ) {
    super(message)
    this.name = 'PolarisError'
    this.kind = kind
    if (options.status !== undefined) this.status = options.status
    this.retryable = options.retryable ?? false
    if (options.path !== undefined) this.path = options.path
    if (options.cause) this.cause = options.cause
  }

  exitCode(): number {
    return this.kind === 'dataset_unavailable' ? 2 : 1
  }
}

export type Result<T> = Promise<T> | T

export function datasetUnavailable(message: string): PolarisError {
  return new PolarisError('dataset_unavailable', message)
}

export function invalidArgument(message: string): PolarisError {
  return new PolarisError('invalid_argument', message)
}

export function lockHeld(path: string): PolarisError {
  return new PolarisError('lock_held', `another sync is already running: ${path}`, { path })
}

export function requestError(
  status: number | undefined,
  message: string,
  retryable = false,
): PolarisError {
  return status === undefined
    ? new PolarisError('request', message, { retryable })
    : new PolarisError('request', message, { status, retryable })
}

export function otherError(message: string, cause?: unknown): PolarisError {
  return new PolarisError('other', message, { cause })
}

export function ensure(condition: unknown, error: PolarisError): asserts condition {
  if (!condition) throw error
}
