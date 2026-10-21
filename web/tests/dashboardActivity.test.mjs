import assert from 'node:assert/strict'
import test from 'node:test'
import { selectCurrentActivity } from '../src/pages/dashboard/requestActivity.ts'

const item = (id, startedAt, outcome = 'running') => ({
  request_id: id, started_at: startedAt, outcome,
})
const older = '2026-10-21T01:00:00Z'
const newer = '2026-10-21T01:00:01Z'

test('an ongoing request takes priority over a newer completed request', () => {
  const running = item('running', older)
  const completed = item('completed', newer, 'success')
  assert.equal(selectCurrentActivity([completed, running]), running)
})

test('multiple ongoing requests show the newest start without changing input order', () => {
  const first = item('first', older)
  const second = item('second', newer)
  const requests = Object.freeze([first, second])
  assert.equal(selectCurrentActivity(requests), second)
  assert.deepEqual(requests, [first, second])
})

test('completion switches to another running request and then the latest request', () => {
  const first = item('first', older)
  const second = item('second', newer)
  assert.equal(selectCurrentActivity([second, first]), second)
  const finished = { ...second, outcome: 'success' }
  assert.equal(selectCurrentActivity([finished, first]), first)
  assert.equal(selectCurrentActivity([{ ...first, outcome: 'failed' }, finished]), finished)
})

test('equal start times select a stable request regardless of snapshot order', () => {
  const first = item('a', older)
  const second = item('b', older)
  assert.equal(selectCurrentActivity([first, second]), second)
  assert.equal(selectCurrentActivity([second, first]), second)
})

test('an empty activity snapshot has no current request', () => {
  assert.equal(selectCurrentActivity([]), undefined)
})
