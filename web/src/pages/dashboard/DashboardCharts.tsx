import { Line } from '@ant-design/plots'
import { Card, Col, Empty, Row, Segmented, Table, Tag, Tooltip, Typography } from 'antd'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import type { DailyBreakdownUsage, Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type BreakdownDimension = 'model' | 'provider'

type TrendMetric = 'requests' | 'tokens' | BreakdownDimension

const BREAKDOWN_COLORS = [
  '#1677ff',
  '#13c2c2',
  '#52c41a',
  '#faad14',
  '#eb2f96',
  '#722ed1',
  '#8c8c8c',
]

const METRIC_OPTIONS = [
  { label: '请求数', value: 'requests' },
  { label: '总 tokens', value: 'tokens' },
  { label: '按模型', value: 'model' },
  { label: '按提供商', value: 'provider' },
]

type BreakdownPoint = {
  day: string
  label: string
  requests: number
}

// Expand the per-day breakdown into a dense day x series matrix so every line
// keeps a point for days it saw no traffic.
function buildBreakdownSeries(
  breakdown: DailyBreakdownUsage[],
  days: Overview['daily_usage'],
): BreakdownPoint[] {
  if (!breakdown.length || !days.length) return []
  const totals = new Map<string, number>()
  for (const row of breakdown) {
    totals.set(row.label, (totals.get(row.label) ?? 0) + row.requests)
  }
  const labels = [...totals.keys()].sort((left, right) => {
    if (left === '其他') return 1
    if (right === '其他') return -1
    return (totals.get(right) ?? 0) - (totals.get(left) ?? 0)
  })
  const byDayLabel = new Map(
    breakdown.map((row) => [`${row.day}\u0000${row.label}`, row.requests]),
  )
  return days.flatMap((day) =>
    labels.map((label) => {
      const requests = byDayLabel.get(`${day.day}\u0000${label}`) ?? 0
      return { day: day.day, label, requests }
    }),
  )
}

type DashboardChartsProps = {
  data: Overview
}

export default function DashboardCharts({ data }: DashboardChartsProps) {
  const navigate = useNavigate()
  const [metric, setMetric] = useState<TrendMetric>('requests')
  const isBreakdown = metric === 'model' || metric === 'provider'
  const points = useMemo(
    () =>
      isBreakdown
        ? buildBreakdownSeries(
            metric === 'model' ? data.daily_model_requests : data.daily_provider_requests,
            data.daily_usage,
          )
        : data.daily_usage,
    [
      data.daily_model_requests,
      data.daily_provider_requests,
      data.daily_usage,
      isBreakdown,
      metric,
    ],
  )
  const valueName = metric === 'tokens' ? '总 tokens' : '请求数'

  return (
    <Row gutter={[16, 16]} className="section-row">
      <Col xs={24} xl={15}>
        <Card title="用量趋势" bordered={false}>
          <Segmented
            block
            size="small"
            value={metric}
            onChange={(value) => setMetric(value as TrendMetric)}
            options={METRIC_OPTIONS}
            style={{ marginBottom: 16 }}
          />
          {points.length ? (
            <Line
              data={points}
              xField="day"
              yField={metric === 'tokens' ? 'tokens' : 'requests'}
              colorField={isBreakdown ? 'label' : undefined}
              height={260}
              axis={{
                x: { title: false },
                y: {
                  title: false,
                  labelFormatter: (value: number) => formatCompact(value),
                },
              }}
              scale={isBreakdown ? { color: { range: BREAKDOWN_COLORS } } : undefined}
              legend={isBreakdown ? { color: { position: 'bottom' } } : undefined}
              style={{ lineWidth: 2 }}
              tooltip={{
                items: [
                  {
                    channel: 'y',
                    name: valueName,
                    valueFormatter: (value: number) => formatExact(value),
                  },
                ],
              }}
            />
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无请求数据" />
          )}
        </Card>
      </Col>
      <Col xs={24} xl={9}>
        <Card title="提供商使用分布" bordered={false} className="full-height-card">
          {data.provider_usage.length ? (
            <Table
              rowKey="provider_id"
              size="small"
              pagination={false}
              dataSource={data.provider_usage}
              onRow={(record) => ({
                onClick: () => navigate(`/usage?provider_id=${record.provider_id}`),
                style: { cursor: 'pointer' },
              })}
              columns={[
                {
                  title: '提供商',
                  dataIndex: 'provider_name',
                  ellipsis: true,
                },
                { title: '请求', dataIndex: 'requests', width: 70 },
                {
                  title: 'tokens',
                  dataIndex: 'tokens',
                  width: 82,
                  render: (value: number, record) => (
                    <Tooltip
                      title={`输入 ${formatExact(record.prompt_tokens)} / 输出 ${formatExact(record.completion_tokens)}`}
                    >
                      <span>{formatCompact(value)}</span>
                    </Tooltip>
                  ),
                },
                {
                  title: '费用',
                  dataIndex: 'cost_micros',
                  width: 86,
                  render: (value: number | null) => formatCostMicros(value),
                },
                {
                  title: '成功率',
                  dataIndex: 'success_rate',
                  width: 90,
                  render: (value: number, record) =>
                    record.requests === 0 ? (
                      <Typography.Text type="secondary">无调用</Typography.Text>
                    ) : (
                      <Tag
                        color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}
                      >
                        {value.toFixed(1)}%
                      </Tag>
                    ),
                },
                {
                  title: '平均总用时',
                  dataIndex: 'avg_latency_ms',
                  width: 90,
                  render: (value: number, record) =>
                    record.requests === 0 ? '-' : `${Math.round(value)} ms`,
                },
              ]}
            />
          ) : (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="请先添加上游提供商"
            />
          )}
        </Card>
      </Col>
    </Row>
  )
}
