import { Line, Pie } from '@ant-design/plots'
import { Card, Col, Empty, Row, Segmented, Space, Tag } from 'antd'
import { useMemo, useState } from 'react'
import type { DailyBreakdownUsage, Overview } from '../../api'
import { formatCompact, formatExact } from '../../format'

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

type DistributionDatum = {
  name: string
  value: number
}

function withAlpha(hex: string, alpha: number) {
  const value = Number.parseInt(hex.slice(1), 16)
  const red = (value >> 16) & 255
  const green = (value >> 8) & 255
  const blue = value & 255
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`
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

function buildDistribution<T>(
  rows: T[],
  getLabel: (row: T) => string,
  getValue: (row: T) => number,
) {
  const sorted = rows
    .filter((row) => getValue(row) > 0)
    .sort((left, right) => getValue(right) - getValue(left))
  if (sorted.length <= 7) {
    return sorted.map((row) => ({ name: getLabel(row), value: getValue(row) }))
  }

  const top = sorted.slice(0, 6)
  const rest = sorted.slice(6).reduce((sum, row) => sum + getValue(row), 0)
  return [
    ...top.map((row) => ({ name: getLabel(row), value: getValue(row) })),
    { name: '其他', value: rest },
  ]
}

function DistributionPie({
  data,
  emptyText,
}: {
  data: DistributionDatum[]
  emptyText: string
}) {
  if (!data.length) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={emptyText} />
  }

  return (
    <Pie
      data={data}
      angleField="value"
      colorField="name"
      innerRadius={0.54}
      height={280}
      scale={{ color: { range: BREAKDOWN_COLORS } }}
      legend={{
        color: {
          position: 'bottom',
          layout: { justifyContent: 'center' },
        },
      }}
      label={false}
      tooltip={{
        items: [
          {
            channel: 'y',
            name: '请求数',
            valueFormatter: (value: number) => formatExact(value),
          },
        ],
      }}
    />
  )
}

type DashboardChartsProps = {
  data: Overview
  providerDataMocked?: boolean
}

export default function DashboardCharts({
  data,
  providerDataMocked = false,
}: DashboardChartsProps) {
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
  const valueColor = metric === 'tokens' ? '#13c2c2' : '#1677ff'
  const valueGradient =
    metric === 'tokens'
      ? 'linear-gradient(-90deg, rgba(19,194,194,0.34) 0%, rgba(19,194,194,0.02) 100%)'
      : 'linear-gradient(-90deg, rgba(22,119,255,0.34) 0%, rgba(22,119,255,0.02) 100%)'
  const breakdownLabels = useMemo(
    () =>
      isBreakdown
        ? [...new Set((points as BreakdownPoint[]).map((point) => point.label))]
        : [],
    [isBreakdown, points],
  )
  const breakdownColorScale = useMemo(
    () => ({ color: { domain: breakdownLabels, range: BREAKDOWN_COLORS } }),
    [breakdownLabels],
  )
  const breakdownColorMap = useMemo(
    () =>
      Object.fromEntries(
        breakdownLabels.map((label, index) => [
          label,
          BREAKDOWN_COLORS[index % BREAKDOWN_COLORS.length],
        ]),
      ),
    [breakdownLabels],
  )
  const breakdownStroke = (group: BreakdownPoint[]) =>
    breakdownColorMap[group[0]?.label] ?? BREAKDOWN_COLORS[0]
  const breakdownFill = (group: BreakdownPoint[]) => {
    const color = breakdownStroke(group)
    return `linear-gradient(-90deg, ${withAlpha(color, 0.3)} 0%, ${withAlpha(color, 0.02)} 100%)`
  }
  const modelPieData = useMemo(
    () =>
      buildDistribution(
        data.model_usage,
        (record) => record.model,
        (record) => record.requests,
      ),
    [data.model_usage],
  )
  const providerPieData = useMemo(
    () =>
      buildDistribution(
        data.provider_usage,
        (record) => record.provider_name,
        (record) => record.requests,
      ),
    [data.provider_usage],
  )

  return (
    <Row gutter={[16, 16]} className="section-row">
      <Col xs={24}>
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
              seriesField={isBreakdown ? 'label' : undefined}
              height={260}
              axis={{
                x: {
                  title: false,
                  labelFormatter: (value: string) =>
                    data.granularity === 'hour' ? value.slice(11, 16) : value,
                },
                y: {
                  title: false,
                  labelFormatter: (value: number) => formatCompact(value),
                },
              }}
              scale={isBreakdown ? breakdownColorScale : undefined}
              legend={isBreakdown ? { color: { position: 'bottom' } } : undefined}
              style={
                isBreakdown
                  ? { lineWidth: 2, stroke: breakdownStroke }
                  : { lineWidth: 2, stroke: valueColor }
              }
              area={
                isBreakdown
                  ? {
                      style: {
                        fill: breakdownFill,
                        fillOpacity: 1,
                      },
                    }
                  : { style: { fill: valueGradient } }
              }
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
      <Col xs={24} xl={12}>
        <Card
          title="模型请求分布"
          bordered={false}
          className="full-height-card"
        >
          <DistributionPie data={modelPieData} emptyText="暂无模型用量" />
        </Card>
      </Col>
      <Col xs={24} xl={12}>
        <Card
          title={
            <Space size={8}>
              <span>提供商请求分布</span>
              {providerDataMocked && <Tag color="gold">演示数据</Tag>}
            </Space>
          }
          bordered={false}
          className="full-height-card"
        >
          <DistributionPie
            data={providerPieData}
            emptyText="请先添加上游提供商"
          />
        </Card>
      </Col>
    </Row>
  )
}
