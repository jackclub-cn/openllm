export type ImportAction = 'create' | 'update' | 'unchanged' | 'skip' | 'error'

export type ImportItem = {
  kind: string
  name: string
  action: ImportAction
  detail: string
}

export type ImportCounts = {
  create: number
  update: number
  unchanged: number
  skip: number
  error: number
}

export type ImportPlan = {
  dry_run: boolean
  applied: boolean
  valid: boolean
  document_version: number
  secrets_included: boolean
  counts: ImportCounts
  items: ImportItem[]
  problems: string[]
  /** Caveats about an applied import (per-key identity, item-by-item writes). */
  notes: string[]
}

/**
 * The portable configuration document. It is intentionally loose on the
 * console side: the page inspects the version and the secret flags, while the
 * server validates the rest.
 */
export type ConfigDocument = {
  openllm_config_version: number
  exported_at: string
  gateway_version: string
  secrets_included: boolean
  notes: string[]
  providers: { name: string }[]
  routes: { name: string }[]
  provider_pools: { name: string }[]
  gateway_keys: { name: string }[]
  settings: Record<string, unknown>
}

export type BackupInfo = {
  name: string
  path: string
  size_bytes: number
  created_at: string
  kind: string
}

export type DatabaseFileReport = {
  file: string
  ok: boolean
  size_bytes: number
  integrity: string
  tables: string[]
  missing_tables: string[]
  applied_migrations: number
  schema_version: number
  providers: number
  routes: number
  api_keys: number
  provider_pools: number
  problems: string[]
}

export type MigrationEntry = {
  version: number
  description: string
}

export type MigrationReport = {
  database: string
  up_to_date: boolean
  applied: MigrationEntry[]
  pending: MigrationEntry[]
  unknown_applied: number[]
  data_present: boolean
  backup_would_be_written: boolean
  problems: string[]
}

export type UpgradeStatus = {
  current_version: string
  latest_version?: string | null
  update_available: boolean
  status: 'up_to_date' | 'update_available' | 'ahead' | 'unknown'
  source: 'query' | 'feed' | 'none'
  checked_at: string
  message: string
  pending_migrations: number
}
