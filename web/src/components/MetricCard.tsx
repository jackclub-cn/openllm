import type { ReactNode } from 'react'
import { Card, Statistic, Tooltip, Typography } from 'antd'
import { compactWithExact, formatExact } from '../format'

type Props = {
  label: string
  value: number
  suffix?: string
  precision?: number
  /**
   * Shorten large values with a K/M/B suffix. The exact figure stays available
   * on hover, so nothing is lost.
   */
  compact?: boolean
  icon: ReactNode
  tone: 'blue' | 'cyan' | 'green' | 'orange' | 'purple'
  hint?: ReactNode
}

export default function MetricCard({
  label,
  value,
  suffix,
  precision,
  compact,
  icon,
  tone,
  hint,
}: Props) {
  // A compact card renders pre-formatted text, so Statistic's own precision and
  // suffix handling must step aside.
  const compacted = compact ? compactWithExact(value) : undefined
  return (
    <Card className="metric-card" bordered={false}>
      <div className={`metric-icon metric-icon-${tone}`}>{icon}</div>
      <div className="metric-copy">
        <Typography.Text type="secondary">{label}</Typography.Text>
        {compacted ? (
          <Tooltip title={formatExact(value)}>
            <div className="metric-compact">
              <Statistic value={compacted.text} />
              {suffix && <span className="metric-suffix">{suffix}</span>}
            </div>
          </Tooltip>
        ) : (
          <Statistic value={value} suffix={suffix} precision={precision} />
        )}
        {hint && (
          <Typography.Text type="secondary" className="metric-hint">
            {hint}
          </Typography.Text>
        )}
      </div>
    </Card>
  )
}
