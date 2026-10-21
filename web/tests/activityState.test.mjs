import assert from 'node:assert/strict'
import test from 'node:test'
import { mergeActivityEvent, mergeActivitySnapshot } from '../src/hooks/activityState.ts'
import { activityElapsedMs, activityLiveTimings } from '../src/hooks/useActivityClock.ts'

const item = (id, revision) => ({ request_id: id, revision, started_at: '2026-10-21T00:00:00Z' })
const snapshot = (sequence, items = []) => ({
  instance_id: 'a', sequence, enabled: true, active_count: 1, generated_at: '', items,
})
const event = (sequence, id, revision) => ({
  instance_id: 'a', sequence, active_count: 0, snapshot: item(id, revision),
})

test('snapshot replays only newer buffered events and preserves revision', () => {
  const result = mergeActivitySnapshot(snapshot(5, [item('r', 3)]), [
    event(6, 'r', 4), event(4, 'r', 2), event(6, 'r', 4), event(7, 'r', 3),
  ])
  assert.equal(result.state.items[0].revision, 4)
  assert.equal(result.state.sequence, 7)
  assert.equal(result.resync, false)
})

test('a missing event requests a snapshot instead of silently dropping another request', () => {
  const result = mergeActivityEvent(snapshot(4, [item('r', 2)]), event(6, 's', 1))
  assert.equal(result.resync, true)
  assert.equal(result.state.sequence, 4)
})

test('restarting the server clears activity from its previous process', () => {
  const result = mergeActivityEvent(snapshot(4, [item('r', 2)]), {
    ...event(1, 's', 1), instance_id: 'b',
  })
  assert.equal(result.state.instance_id, 'b')
  assert.deepEqual(result.state.items, [])
  assert.equal(result.resync, true)
})

test('snapshot reconciliation preserves unrelated newer requests and authoritative active count', () => {
  const result = mergeActivitySnapshot(snapshot(3, [item('r', 1)]), [
    event(4, 's', 1), { ...event(5, 't', 1), active_count: 6 },
  ])
  assert.equal(result.state.items.length, 3)
  assert.equal(result.state.active_count, 6)
})

test('live durations use monotonic increments and advance only the current phase', () => {
  const request = {
    outcome: 'running', phase: 'streaming', started_at: '2099-01-01T00:00:00Z',
    streamed: true, timings: { total_ms: 1000, prepare_ms: 100, generation_ms: 200 },
  }
  assert.equal(activityElapsedMs(request, 0), 1000)
  const now = performance.now() + 1000
  const total = activityElapsedMs(request, now)
  const timings = activityLiveTimings(request, now)
  assert.ok(total >= 2000)
  assert.equal(timings.prepare_ms, 100)
  assert.equal(timings.generation_ms, 200 + total - 1000)
})

test('completed durations stop advancing', () => {
  const request = {
    outcome: 'success', phase: 'completed', timings: { total_ms: 1000, generation_ms: 600 },
  }
  assert.equal(activityElapsedMs(request, performance.now() + 100000), 1000)
  assert.equal(activityLiveTimings(request, performance.now() + 100000).generation_ms, 600)
})
