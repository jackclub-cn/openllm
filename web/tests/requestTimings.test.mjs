import assert from 'node:assert/strict'
import test from 'node:test'
import { analyzeRequestTimings } from '../src/components/requestTimings.ts'

test('streamed requests attribute measured delay without counting TTFT twice', () => {
  const result = analyzeRequestTimings({
    total_ms: 10000, prepare_ms: 100, route_wait_ms: 900,
    upstream_headers_ms: 1000, response_wait_ms: 6000,
    generation_ms: 2000, time_to_first_token_ms: 8000,
  }, true)
  assert.equal(result.dominant.key, 'first_output')
  assert.equal(result.dominant.value, 6000)
  assert.equal(result.unaccounted, 0)
})

test('nonstreamed requests show response reads without invented generation stages', () => {
  const result = analyzeRequestTimings({
    total_ms: 5000, response_read_ms: 4500, time_to_first_token_ms: 5000,
  }, false)
  assert.equal(result.dominant.key, 'response')
  assert.equal(result.unaccounted, 500)
  assert.equal(result.stages.some((stage) => stage.key === 'generation' || stage.key === 'first_output'), false)
})

test('missing or zero measurements do not produce a guessed cause', () => {
  assert.equal(analyzeRequestTimings({ total_ms: 5000 }, true).dominant, undefined)
  assert.equal(analyzeRequestTimings({ total_ms: 5000, prepare_ms: 0 }, true).dominant, undefined)
  assert.equal(analyzeRequestTimings({ total_ms: 5000 }, true).unaccounted, 5000)
})

test('preparation or capacity waiting can dominate without blaming the provider', () => {
  const result = analyzeRequestTimings({
    total_ms: 5000, prepare_ms: 100, route_wait_ms: 4900,
  }, true)
  assert.equal(result.dominant.key, 'routing')
})
