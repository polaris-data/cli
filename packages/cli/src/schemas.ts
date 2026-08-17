import { z } from 'incur'

export const remoteDatasetSchema = z.object({
  source: z.string(),
  market: z.string(),
  start: z.string(),
  end: z.string(),
  catalog_source: z.string().nullable(),
  access: z
    .object({
      status: z.enum(['open', 'preview', 'restricted']),
      public_cutoff_date: z.string().nullable(),
    })
    .nullable(),
  categories: z.array(z.string()).optional(),
  dataset: z.string(),
})

export const remoteListOutputSchema = z.object({
  command: z.literal('catalog'),
  filters: z.object({
    source: z.string().nullable(),
    market: z.string().nullable(),
    search: z.string().nullable(),
  }),
  dataset_total: z.number(),
  datasets: z.array(remoteDatasetSchema),
})

export const localSnapshotSchema = z.object({
  key: z.string(),
  path: z.string(),
  filename: z.string(),
  source: z.string().nullable(),
  market: z.string().nullable(),
  date: z.string().nullable(),
})

export const localListOutputSchema = z.object({
  command: z.literal('list'),
  root: z.string(),
  filters: z.object({
    source: z.string().nullable(),
    market: z.string().nullable(),
    date: z.string().nullable(),
  }),
  snapshot_total: z.number(),
  snapshots: z.array(localSnapshotSchema),
})

export const syncOutputSchema = z.object({
  command: z.literal('download'),
  source: z.string(),
  market: z.string(),
  requested_range: z.object({ from: z.string(), to: z.string() }),
  effective_range: z.object({ from: z.string(), to: z.string() }),
  root: z.string(),
  remote_total: z.number(),
  downloaded_total: z.number(),
  skipped_total: z.number(),
  failed_total: z.number(),
  downloaded_keys: z.array(z.string()),
  failed: z.array(z.object({ key: z.string(), error: z.string() })),
})

export const resetOutputSchema = z.object({
  command: z.literal('reset'),
  root: z.string(),
  snapshot_total: z.number(),
  removed_roots: z.array(z.string()),
})

export const accountOutputSchema = z.object({
  base_url: z.string(),
  auth: z.string(),
  status: z.string(),
  user_id: z.string().nullable(),
  email: z.string().nullable(),
  plan: z.string().nullable(),
  provider: z.string().nullable(),
  key_id: z.string().nullable(),
})

export const loginOutputSchema = z.object({
  status: z.literal('signed_in'),
  user_id: z.string(),
  display_name: z.string().nullable(),
  email: z.string().nullable(),
  plan: z.string().nullable(),
})

export type RemoteDatasetEntry = z.infer<typeof remoteDatasetSchema>
export type RemoteListOutput = z.infer<typeof remoteListOutputSchema>
export type LocalSnapshot = z.infer<typeof localSnapshotSchema>
export type LocalListOutput = z.infer<typeof localListOutputSchema>
export type SyncOutput = z.infer<typeof syncOutputSchema>
export type ResetOutput = z.infer<typeof resetOutputSchema>
export type AccountOutput = z.infer<typeof accountOutputSchema>
export type LoginOutput = z.infer<typeof loginOutputSchema>
