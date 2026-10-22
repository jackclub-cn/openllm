import type { Dayjs } from 'dayjs'
import type { UsageStatusFilter } from './types'

export type UsageQueryFilters = {
  model: string
  requestId: string
  sessionId: string
  endpoint: string
  providerId?: number
  providerApiKeyId?: number
  apiKeyId?: number
  routeId?: number
  statusFilter: UsageStatusFilter
  onlyAdjusted: boolean
  dates?: [Dayjs, Dayjs]
}

export function usageFilterParams(filters: UsageQueryFilters) {
  const params = new URLSearchParams()
  if (filters.model) params.set('model', filters.model)
  if (filters.requestId) params.set('request_id', filters.requestId)
  if (filters.sessionId) params.set('session_id', filters.sessionId)
  if (filters.endpoint) params.set('endpoint', filters.endpoint)
  if (filters.providerId) params.set('provider_id', String(filters.providerId))
  if (filters.providerApiKeyId) params.set('provider_api_key_id', String(filters.providerApiKeyId))
  if (filters.apiKeyId) params.set('api_key_id', String(filters.apiKeyId))
  if (filters.routeId) params.set('route_id', String(filters.routeId))
  if (filters.statusFilter === 'success') params.set('success', 'true')
  if (filters.statusFilter === 'failed') params.set('success', 'false')
  if (filters.statusFilter === 'pending') params.set('in_flight', 'true')
  if (filters.onlyAdjusted) params.set('gateway_adjusted', 'true')
  if (filters.dates?.[0]) params.set('from', filters.dates[0].toISOString())
  if (filters.dates?.[1]) params.set('to', filters.dates[1].toISOString())
  return params
}
