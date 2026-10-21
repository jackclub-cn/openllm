import type {
  CooldownSnapshot,
  RuntimeLimits,
  RuntimeStatus,
  Settings,
} from './runtime'
import type { PoolUsage } from './pools'
import type { Provider } from './providers'

export type SelfCheckStatus = 'pass' | 'warn' | 'fail'

export type SelfCheckItem = {
  id: string
  category: string
  title: string
  status: SelfCheckStatus
  detail: string
  hint?: string
}

export type SelfCheckReport = {
  generated_at: string
  overall: SelfCheckStatus
  counts: { pass: number; warn: number; fail: number }
  checks: SelfCheckItem[]
}

export type DiagnosticFailure = {
  request_id: string
  created_at: string
  requested_model: string
  upstream_model?: string | null
  provider_id?: number | null
  endpoint: string
  streamed: boolean
  status_code: number
  latency_ms: number
  error_message?: string | null
  warning_message?: string | null
}

export type DiagnosticSnapshot = {
  generated_at: string
  version: string
  host: string
  draining: boolean
  uptime_seconds: number
  instance_lease?: {
    id: string
    pid: number
    host: string
    started_at: string
    heartbeat_at: string
    heartbeat_age_seconds: number
    ours: boolean
  } | null
  status: RuntimeStatus
  limits: RuntimeLimits
  settings: Settings
  self_check: SelfCheckReport
  resilience: CooldownSnapshot
  pools: PoolUsage[]
  providers: Provider[]
  recent_failures: DiagnosticFailure[]
  requests_24h: {
    total: number
    successful: number
    failed: number
    failures_by_status: { status_code: number; count: number }[]
  }
  secrets_included: boolean
  notes: string[]
}
