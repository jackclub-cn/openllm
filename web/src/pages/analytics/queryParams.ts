import type { Dayjs } from 'dayjs'
import type { AnalyticsGranularity } from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'

/**
 * One place to translate the visible analytics filters into query params, so
 * the overview and the drill-down tables can never drift apart.
 */
export function buildAnalyticsParams(
  dates: [Dayjs, Dayjs],
  filters: UsageDimensionFilterValues,
  granularity: AnalyticsGranularity = 'auto',
): URLSearchParams {
  const params = new URLSearchParams({
    tz_offset_minutes: String(-new Date().getTimezoneOffset()),
    from: dates[0].startOf('day').toISOString(),
    to: dates[1].add(1, 'day').startOf('day').toISOString(),
    include_session_metrics: 'true',
  })
  if (filters.model.trim()) params.set('model', filters.model.trim())
  if (filters.endpoint.trim()) params.set('endpoint', filters.endpoint.trim())
  if (filters.providerId) params.set('provider_id', String(filters.providerId))
  if (filters.providerApiKeyId) {
    params.set('provider_api_key_id', String(filters.providerApiKeyId))
  }
  if (filters.apiKeyId) params.set('api_key_id', String(filters.apiKeyId))
  if (filters.routeId) params.set('route_id', String(filters.routeId))
  if (filters.statusFilter === 'success') params.set('success', 'true')
  if (filters.statusFilter === 'failed') params.set('success', 'false')
  if (granularity !== 'auto') params.set('granularity', granularity)
  if (filters.onlyAdjusted) params.set('gateway_adjusted', 'true')
  return params
}
