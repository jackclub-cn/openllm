import type { RequestActivity } from '../../api/activity'

export function selectCurrentActivity(items: readonly RequestActivity[]) {
  return items.reduce<RequestActivity | undefined>((current, item) => {
    if (!current) return item
    const running = item.outcome === 'running'
    const currentRunning = current.outcome === 'running'
    if (running !== currentRunning) return running ? item : current
    const order = item.started_at.localeCompare(current.started_at)
      || item.request_id.localeCompare(current.request_id)
    return order > 0 ? item : current
  }, undefined)
}
