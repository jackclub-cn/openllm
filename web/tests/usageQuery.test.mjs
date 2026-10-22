import assert from 'node:assert/strict'
import test from 'node:test'
import dayjs from 'dayjs'
import { usageFilterParams } from '../src/pages/usage/query.ts'

const defaults = {
  model: '', requestId: '', sessionId: '', endpoint: '',
  statusFilter: 'all', onlyAdjusted: false,
}

test('a minute-long investigation preserves exact instants in a non-UTC timezone', () => {
  const params = usageFilterParams({
    ...defaults,
    dates: [
      dayjs('2026-10-22T16:23:14.123+08:00'),
      dayjs('2026-10-22T16:24:15.456+08:00'),
    ],
  })
  assert.equal(params.get('from'), '2026-10-22T08:23:14.123Z')
  assert.equal(params.get('to'), '2026-10-22T08:24:15.456Z')
})

test('a window crossing midnight is not expanded to either whole day', () => {
  const params = usageFilterParams({
    ...defaults,
    dates: [
      dayjs('2026-10-22T23:59:57+08:00'),
      dayjs('2026-10-23T00:00:03+08:00'),
    ],
  })
  assert.equal(params.get('from'), '2026-10-22T15:59:57.000Z')
  assert.equal(params.get('to'), '2026-10-22T16:00:03.000Z')
})

test('clearing the time window omits both bounds and paging is not part of exports', () => {
  assert.equal(usageFilterParams(defaults).toString(), '')
})

test('query, URL and export filters retain every non-time dimension', () => {
  const params = usageFilterParams({
    ...defaults,
    model: 'org/model', requestId: 'request-1', sessionId: 'session-1',
    endpoint: '/v1/responses', providerId: 2, providerApiKeyId: 3,
    apiKeyId: 4, routeId: 5, onlyAdjusted: true, statusFilter: 'failed',
  })
  assert.deepEqual(Object.fromEntries(params), {
    model: 'org/model', request_id: 'request-1', session_id: 'session-1',
    endpoint: '/v1/responses', provider_id: '2', provider_api_key_id: '3',
    api_key_id: '4', route_id: '5', success: 'false', gateway_adjusted: 'true',
  })
  assert.equal(usageFilterParams({ ...defaults, statusFilter: 'success' }).get('success'), 'true')
  assert.equal(usageFilterParams({ ...defaults, statusFilter: 'pending' }).get('in_flight'), 'true')
})
