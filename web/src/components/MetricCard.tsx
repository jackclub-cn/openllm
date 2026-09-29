import type { ReactNode } from 'react'
import { Card, Statistic, Typography } from 'antd'

type Props = {
  label: string
  value: number
  suffix?: string
  precision?: number
  icon: ReactNode
  tone: 'blue' | 'cyan' | 'green' | 'orange' | 'purple'
}

export default function MetricCard({ label, value, suffix, precision, icon, tone }: Props) {
  return (
    <Card className="metric-card" bordered={false}>
      <div className={`metric-icon metric-icon-${tone}`}>{icon}</div>
      <div className="metric-copy">
        <Typography.Text type="secondary">{label}</Typography.Text>
        <Statistic value={value} suffix={suffix} precision={precision} />
      </div>
    </Card>
  )
}
