import {
  ArrowDownOutlined,
  ArrowUpOutlined,
  MinusOutlined,
} from '@ant-design/icons'
import type { ReactNode } from 'react'
import { Card, Statistic, Tooltip, Typography } from 'antd'
import { compactWithExact, formatExact } from '../format'

type Comparison = {
  previous?: number | null
  delta?: 'percent' | 'point'
  lowerIsBetter?: boolean
  previousLabel?: string
}

type Props = {
  label: string
  value?: number | null
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
  comparison?: Comparison
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
  comparison,
}: Props) {
  // A compact card renders pre-formatted text, so Statistic's own precision and
  // suffix handling must step aside.
  const hasValue = value != null && Number.isFinite(value)
  const compacted = compact && hasValue ? compactWithExact(value) : undefined
  const comparisonView = (() => {
    if (
      !comparison ||
      typeof value !== 'number' ||
      !Number.isFinite(value) ||
      comparison.previous == null ||
      !Number.isFinite(comparison.previous)
    ) {
      return null
    }
    const previous = comparison.previous
    if (previous === 0) {
      return value === 0
        ? { label: '较上期持平', color: undefined, icon: <MinusOutlined /> }
        : { label: '上期无数据', color: undefined, icon: <MinusOutlined /> }
    }
    const delta =
      comparison.delta === 'point'
        ? value - previous
        : ((value - previous) / Math.abs(previous)) * 100
    if (Math.abs(delta) < 0.05) {
      return { label: '较上期持平', color: undefined, icon: <MinusOutlined /> }
    }
    const improving = comparison.lowerIsBetter ? delta < 0 : delta > 0
    return {
      label: `较上期 ${delta > 0 ? '+' : ''}${delta.toFixed(1)}${
        comparison.delta === 'point' ? ' pp' : '%'
      }`,
      color: improving ? '#389e0d' : '#cf1322',
      icon: delta > 0 ? <ArrowUpOutlined /> : <ArrowDownOutlined />,
    }
  })()
  return (
    <Card className="metric-card" bordered={false}>
      <div className={`metric-icon metric-icon-${tone}`}>{icon}</div>
      <div className="metric-copy">
        <div className="metric-label-row">
          <Typography.Text type="secondary">{label}</Typography.Text>
          {comparisonView && (
            <Tooltip
              title={
                comparison?.previousLabel ??
                `上一周期 ${formatExact(comparison?.previous ?? 0)}`
              }
            >
              <span
                className="metric-comparison"
                style={comparisonView.color ? { color: comparisonView.color } : undefined}
              >
                {comparisonView.icon}
                {comparisonView.label}
              </span>
            </Tooltip>
          )}
        </div>
        {!hasValue ? (
          <Statistic value="-" />
        ) : compacted ? (
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
