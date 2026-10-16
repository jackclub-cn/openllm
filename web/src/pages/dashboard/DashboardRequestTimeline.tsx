import { useEffect, useState } from 'react'
import { Alert, Card, Space, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import type { UsageLog } from '../../api'

type DashboardRequestTimelineProps = {
  items: UsageLog[]
  error?: string
  onOpenDetail: (record: UsageLog) => void
}

type TimelineEntry = {
  record: UsageLog
  start: number
  end: number
  lane: number
}

const WINDOW_MS = 60_000
const LANE_HEIGHT = 24
const BAR_HEIGHT = 14

function formatDuration(value: number) {
  return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${Math.round(value)} ms`
}

function buildTimeline(items: UsageLog[], now: number) {
  const domainStart = now - WINDOW_MS
  const domainEnd = now
  const visible = items
    .map((record) => {
      const start = dayjs(record.created_at).valueOf()
      if (!Number.isFinite(start)) return undefined
      const end = record.in_flight
        ? now
        : start + Math.max(record.latency_ms || 0, 1)
      return { record, start, end, lane: 0 }
    })
    .filter(
      (entry): entry is TimelineEntry =>
        Boolean(entry && entry.end >= domainStart && entry.start <= domainEnd),
    )
    .sort((left, right) => left.start - right.start || left.end - right.end)

  const laneEnds: number[] = []
  for (const entry of visible) {
    let lane = laneEnds.findIndex((end) => end + 20 <= entry.start)
    if (lane === -1) {
      lane = laneEnds.length
      laneEnds.push(entry.end)
    } else {
      laneEnds[lane] = entry.end
    }
    entry.lane = lane
  }

  const tickCount = 6
  const ticks = Array.from(
    { length: tickCount },
    (_, index) => domainStart + (WINDOW_MS * index) / (tickCount - 1),
  )

  return {
    entries: visible,
    ticks,
    domainStart,
    domainEnd,
    laneCount: Math.max(laneEnds.length, 1),
  }
}

function barClass(record: UsageLog) {
  if (record.in_flight) return 'usage-timeline-bar-pending'
  if (!record.success) return 'usage-timeline-bar-error'
  if (record.warning_message) return 'usage-timeline-bar-warning'
  return 'usage-timeline-bar-success'
}

function requestStatus(record: UsageLog) {
  if (record.in_flight) return '请求中'
  return record.success ? '成功' : `失败 ${record.status_code}`
}

export default function DashboardRequestTimeline({
  items,
  error,
  onOpenDetail,
}: DashboardRequestTimelineProps) {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])

  const { entries, ticks, domainStart, domainEnd, laneCount } = buildTimeline(
    items,
    now,
  )
  const span = domainEnd - domainStart
  const plotHeight = Math.max(laneCount * LANE_HEIGHT, 72)

  const percent = (value: number) =>
    Math.max(0, Math.min(100, ((value - domainStart) / span) * 100))

  return (
    <Card
      className="section-row usage-timeline-card"
      bordered={false}
      title="请求时间轴"
      extra={
        <div className="usage-timeline-controls">
          <Space size={12} wrap>
            <span className="usage-timeline-legend">
              <i className="usage-timeline-dot-success" />
              成功
            </span>
            <span className="usage-timeline-legend">
              <i className="usage-timeline-dot-warning" />
              告警
            </span>
            <span className="usage-timeline-legend">
              <i className="usage-timeline-dot-error" />
              失败
            </span>
            <span className="usage-timeline-legend">
              <i className="usage-timeline-dot-pending" />
              请求中
            </span>
          </Space>
          <Typography.Text type="secondary">
            最近 60 秒 · {entries.length} 条
          </Typography.Text>
        </div>
      }
    >
      {error && (
        <Alert
          type="warning"
          showIcon
          message="无法刷新请求时间轴"
          description={error}
        />
      )}
      <div className="usage-timeline-canvas" style={{ height: 36 + plotHeight }}>
        <div className="usage-timeline-axis">
          {ticks.map((tick) => (
            <span key={tick} style={{ left: `${percent(tick)}%` }}>
              {dayjs(tick).format('HH:mm:ss')}
            </span>
          ))}
        </div>
        <div className="usage-timeline-plot" style={{ height: plotHeight }}>
          {ticks.map((tick) => (
            <i key={tick} style={{ left: `${percent(tick)}%` }} />
          ))}
          {entries.map((entry) => {
            const left = percent(Math.max(entry.start, domainStart))
            const right = percent(Math.min(entry.end, domainEnd))
            const width = Math.max(right - left, 0.35)
            const duration = entry.end - entry.start
            const record = entry.record
            return (
              <Tooltip
                key={record.id}
                title={
                  <div className="usage-timeline-tooltip">
                    <strong>{record.requested_model}</strong>
                    <span>
                      {requestStatus(record)} · {formatDuration(duration)}
                    </span>
                    <span>{dayjs(entry.start).format('YYYY-MM-DD HH:mm:ss.SSS')}</span>
                    <span>
                      {record.provider_name ||
                        (record.provider_id ? `#${record.provider_id}` : '未知提供商')}
                      {record.route_name ? ` · ${record.route_name}` : ''}
                    </span>
                  </div>
                }
              >
                <button
                  type="button"
                  className={`usage-timeline-bar ${barClass(record)}`}
                  aria-label={`${record.requested_model} ${formatDuration(duration)}`}
                  style={{
                    left: `${left}%`,
                    width: `${Math.min(width, 100 - left)}%`,
                    top: entry.lane * LANE_HEIGHT + (LANE_HEIGHT - BAR_HEIGHT) / 2,
                    height: BAR_HEIGHT,
                  }}
                  onClick={() => onOpenDetail(record)}
                />
              </Tooltip>
            )
          })}
        </div>
      </div>
    </Card>
  )
}
