import { Space, Tag, Typography } from 'antd'
import type { ActivityTimings } from '../api/activity'
import type { RequestPerformance } from '../api/usage'
import { analyzeRequestTimings, formatTimingMs } from './requestTimings'

export default function RequestTimingPanel({
  timings,
  streamed,
  performance,
  running = false,
}: {
  timings: ActivityTimings
  streamed: boolean
  performance?: RequestPerformance | null
  running?: boolean
}) {
  const { stages, dominant, unaccounted } = analyzeRequestTimings(timings, streamed)
  const total = Math.max(timings.total_ms, 1)

  return (
    <section className="request-timing-panel">
      <Typography.Title level={5}>阶段耗时</Typography.Title>
      <Space size={8} wrap className="request-timing-summary">
        <Tag>{running ? '已运行' : '总用时'} {formatTimingMs(timings.total_ms)}</Tag>
        {dominant && (
          <Typography.Text>
            {running ? '当前主要耗时' : '主要耗时'}：{dominant.label}
            {' '}· {formatTimingMs(dominant.value)}
            {' '}（{Math.min(100, dominant.value! / total * 100).toFixed(1)}%）
          </Typography.Text>
        )}
      </Space>
      <div className="activity-timings">
        {stages.map((stage) => (
          <div key={stage.key} className="activity-timing-row">
            <span>{stage.label}</span>
            <div className="activity-timing-track">
              <i style={{
                width: `${stage.value ? Math.min(100, Math.max(1, stage.value / total * 100)) : 0}%`,
                background: stage.color,
              }} />
            </div>
            <strong>{formatTimingMs(stage.value)}</strong>
          </div>
        ))}
      </div>
      {unaccounted > Math.max(10, total * 0.05) && (
        <Typography.Text type="secondary">未归因耗时 {formatTimingMs(unaccounted)}</Typography.Text>
      )}
      {performance && performance.attempt_count > 0 && (
        <Space size={8} wrap className="request-timing-summary">
          <Tag>尝试 {performance.attempt_count} 次</Tag>
          <Tag color={performance.fallback_count > 0 ? 'orange' : 'default'}>
            回退 {performance.fallback_count} 次
          </Tag>
          {performance.failed_attempts_ms > 0 && (
            <Typography.Text type="secondary">
              {performance.attempts_truncated ? '已保留的' : ''}失败尝试耗时
              {' '}{formatTimingMs(performance.failed_attempts_ms)}
            </Typography.Text>
          )}
        </Space>
      )}
    </section>
  )
}
