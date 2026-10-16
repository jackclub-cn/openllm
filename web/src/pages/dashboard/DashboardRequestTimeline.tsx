import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
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
const TICK_MS = 10_000
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

  // Ticks land on absolute ten-second boundaries, so a label keeps its time and
  // scrolls through the window instead of being relabelled in place every
  // second. One tick past each edge keeps the ruler covered while it slides.
  const ticks: number[] = []
  for (
    let tick = Math.floor(domainStart / TICK_MS) * TICK_MS;
    tick <= domainEnd + TICK_MS;
    tick += TICK_MS
  ) {
    ticks.push(tick)
  }

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
  // `now` is the render-time anchor: every second the window is rebuilt
  // against a fresh timestamp. Between anchors the whole plot is slid by a
  // per-frame transform, so the timeline glides instead of hopping once a
  // second.
  const [now, setNow] = useState(() => Date.now())
  const nowRef = useRef(now)
  const axisLayerRef = useRef<HTMLDivElement>(null)
  const plotLayerRef = useRef<HTMLDivElement>(null)
  const pendingBarsRef = useRef<HTMLElement[]>([])

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])

  const slideWindow = useCallback(() => {
    const plot = plotLayerRef.current
    const axis = axisLayerRef.current
    if (!plot || !axis || !plot.clientWidth) return
    const elapsed = Math.min(Math.max(Date.now() - nowRef.current, 0), WINDOW_MS)
    const offset = (elapsed / WINDOW_MS) * plot.clientWidth
    const transform = `translate3d(${-offset}px, 0, 0)`
    plot.style.transform = transform
    axis.style.transform = transform
    // An in-flight request ends at "now", so its right edge has to stay pinned
    // to the right side of the window while everything else slides under it.
    for (const bar of pendingBarsRef.current) {
      bar.style.right = `${-offset}px`
    }
  }, [])

  // Runs before paint, so re-anchoring never shows a one-frame jump.
  useLayoutEffect(() => {
    nowRef.current = now
    pendingBarsRef.current = Array.from(
      plotLayerRef.current?.querySelectorAll<HTMLElement>(
        '.usage-timeline-bar-pending',
      ) ?? [],
    )
    slideWindow()
  }, [now, items, slideWindow])

  useEffect(() => {
    let handle = 0
    const step = () => {
      if (!document.hidden) {
        // A backgrounded tab throttles the one-second anchor; re-anchor in one
        // go instead of letting the plot fly off its window.
        if (Date.now() - nowRef.current > 2_000) setNow(Date.now())
        else slideWindow()
      }
      handle = window.requestAnimationFrame(step)
    }
    handle = window.requestAnimationFrame(step)
    return () => window.cancelAnimationFrame(handle)
  }, [slideWindow])

  const { entries, ticks, domainStart, domainEnd, laneCount } = buildTimeline(
    items,
    now,
  )
  const span = domainEnd - domainStart
  const plotHeight = Math.max(laneCount * LANE_HEIGHT, 72)

  // Positions stay unclamped: bars and ticks that reach past either edge have
  // to keep moving with the window and let the plot clip them, otherwise they
  // would sit still between re-anchors and lurch on every one.
  const position = (value: number) => ((value - domainStart) / span) * 100

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
          <div className="usage-timeline-layer" ref={axisLayerRef}>
            {ticks.map((tick) => (
              <span key={tick} style={{ left: `${position(tick)}%` }}>
                {dayjs(tick).format('HH:mm:ss')}
              </span>
            ))}
          </div>
        </div>
        <div className="usage-timeline-plot" style={{ height: plotHeight }}>
          <div className="usage-timeline-layer" ref={plotLayerRef}>
            {ticks.map((tick) => (
              <i
                key={tick}
                className="usage-timeline-gridline"
                style={{ left: `${position(tick)}%` }}
              />
            ))}
            {entries.map((entry) => {
              const left = position(entry.start)
              const width = Math.max(position(entry.end) - left, 0.35)
              const duration = entry.end - entry.start
              const record = entry.record
              const top =
                entry.lane * LANE_HEIGHT + (LANE_HEIGHT - BAR_HEIGHT) / 2
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
                    style={
                      // In-flight bars are stretched to the live right edge by
                      // the animation loop; finished bars keep a fixed width.
                      record.in_flight
                        ? { left: `${left}%`, right: 0, top, height: BAR_HEIGHT }
                        : { left: `${left}%`, width: `${width}%`, top, height: BAR_HEIGHT }
                    }
                    onClick={() => onOpenDetail(record)}
                  />
                </Tooltip>
              )
            })}
          </div>
        </div>
      </div>
    </Card>
  )
}
