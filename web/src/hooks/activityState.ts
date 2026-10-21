import type { ActivityEvent, ActivityList, RequestActivity } from '../api/activity'

export function sortActivity(items: RequestActivity[]) {
  return items.sort((a, b) => b.started_at.localeCompare(a.started_at)
    || b.request_id.localeCompare(a.request_id)).slice(0, 50)
}

export function mergeActivityEvent(current: ActivityList, event: ActivityEvent) {
  if (current.instance_id && current.instance_id !== event.instance_id) {
    return { state: { ...current, instance_id: event.instance_id, sequence: 0, items: [], active_count: 0 }, resync: true }
  }
  if (event.sequence <= current.sequence) return { state: current, resync: false }
  if (event.sequence !== current.sequence + 1) return { state: current, resync: true }
  const items = new Map(current.items.map((item) => [item.request_id, item]))
  const old = items.get(event.snapshot.request_id)
  if (!old || old.revision < event.snapshot.revision) items.set(event.snapshot.request_id, event.snapshot)
  return {
    state: { ...current, instance_id: event.instance_id, sequence: event.sequence,
      active_count: event.active_count, items: sortActivity([...items.values()]) },
    resync: false,
  }
}

export function mergeActivitySnapshot(snapshot: ActivityList, buffered: ActivityEvent[]) {
  let state = snapshot
  let resync = false
  for (const event of [...buffered].sort((a, b) => a.sequence - b.sequence)) {
    if (event.instance_id !== snapshot.instance_id) { resync = true; continue }
    const merged = mergeActivityEvent(state, event)
    state = merged.state
    resync ||= merged.resync
  }
  return { state, resync }
}
