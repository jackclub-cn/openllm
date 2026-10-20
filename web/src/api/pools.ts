export type PoolAllocationUsage = {
  api_key_id: number
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
  request_share?: number | null
  token_share?: number | null
  key_requests: number
  key_tokens: number
}

export type PoolUsage = {
  pool_id: number
  provider_id: number
  name: string
  window_seconds: number
  request_limit?: number | null
  token_limit?: number | null
  requests: number
  tokens: number
  saturated: boolean
  denials: number
  allocations: PoolAllocationUsage[]
}

/// One gateway key's reserved share of a provider pool.
export type ProviderPoolAllocation = {
  id: number
  api_key_id: number
  api_key_name?: string | null
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
  /// Effective capacity reserved for the key, in the pool's own units.
  request_share?: number | null
  token_share?: number | null
}

/// A declared share of one provider's capacity, as returned by the API.
export type ProviderPool = {
  id: number
  provider_id: number
  provider_name?: string | null
  name: string
  enabled: boolean
  window_seconds: number
  request_limit?: number | null
  token_limit?: number | null
  saturation_threshold_pct: number
  allocations: ProviderPoolAllocation[]
  created_at: string
  updated_at: string
}

export type ProviderPoolAllocationInput = {
  api_key_id: number
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
}

/// Create/update payload. Create requires `provider_id`; update omits it and
/// leaves any field absent from the body unchanged.
export type ProviderPoolInput = {
  provider_id?: number
  name?: string
  enabled?: boolean
  window_seconds?: number
  request_limit?: number | null
  token_limit?: number | null
  saturation_threshold_pct?: number
  allocations?: ProviderPoolAllocationInput[]
}
