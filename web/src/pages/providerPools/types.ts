import type { ProviderPoolAllocationInput } from '../../api'

export type AllocationPolicy = ProviderPoolAllocationInput['policy']

export type ProviderPoolAllocationFormValue = {
  api_key_id?: number
  weight_pct?: number
  policy?: AllocationPolicy
}

export type ProviderPoolFormValues = {
  provider_id?: number
  name: string
  enabled: boolean
  window_seconds: number
  request_limit?: number | null
  token_limit?: number | null
  saturation_threshold_pct: number
  allocations: ProviderPoolAllocationFormValue[]
}

/// Backend `window_seconds` bounds, mirrored so the form cannot submit a value
/// the API would reject outright. The server stays authoritative.
export const MIN_POOL_WINDOW_SECONDS = 1
export const MAX_POOL_WINDOW_SECONDS = 86_400

export const allocationPolicyOptions: Array<{
  value: AllocationPolicy
  label: string
}> = [
  { value: 'hard', label: 'hard · 超出份额直接拒绝' },
  { value: 'soft', label: 'soft · 超出份额只降权' },
  { value: 'burst', label: 'burst · 仍可使用剩余余量' },
]
