import { Line, Pie } from '@ant-design/plots'
import {
  Card,
  Col,
  Empty,
  Row,
  Segmented,
  Select,
  Space,
  Tag,
  Typography,
} from 'antd'
import { useMemo, useState } from 'react'
import type { DailyBreakdownUsage, Overview } from '../../api'
import { formatCompact, formatExact } from '../../format'

type BreakdownDimension = 'model' | 'provider'
type TrendView = 'total' | BreakdownDimension
type TrendMetric = 'requests' | 'tokens' | 'cost' | 'success' | 'cache'

const BREAKDOWN_COLORS = [
  '#1677ff',
  '#13c2c2',
  '#52c41a',
  '#faad14',
  '#eb2f96',
  '#722ed1',
  '#8c8c8c',
]

const VIEW_OPTIONS = [
  { label: '不拆分', value: 'total' },
  { label: '按模型', value: 'model' },
  { label: '按提供商', value: 'provider' },
]

const METRIC_OPTIONS = [
  { label: '请求', value: 'requests' },
  { label: 'Token', value: 'tokens' },
  { label: '费用', value: 'cost' },
  { label: '成功', value: 'success' },
  { label: '缓存', value: 'cache' },
]

type SeriesPoint = {
  day: string
  label: string
  value: number
}

type DistributionDatum = {
  name: string
  value: number
}

const METRIC_STYLE: Record<
  TrendMetric,
  { name: string; color: string; format: (value: number) => string }
> = {
  requests: {
    name: '请求数',
    color: '#1677ff',
    format: (value) => formatExact(value),
  },
  tokens: {
    name: '总 tokens',
    color: '#13c2c2',
    format: (value) => formatExact(value),
  },
  cost: {
    name: '费用',
    color: '#fa8c16',
    format: (value) => `$${value.toFixed(value < 0.01 ? 4 : 2)}`,
  },
  success: {
    name: '成功率',
    color: '#52c41a',
    format: (value) => `${value.toFixed(1)}%`,
  },
  cache: {
    name: '缓存命中率',
    color: '#2f54eb',
    format: (value) => `${value.toFixed(1)}%`,
  },
}

function withAlpha(hex: string, alpha: number) {
  const value = Number.parseInt(hex.slice(1), 16)
  const red = (value >> 16) & 255
  const green = (value >> 8) & 255
  const blue = value & 255
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`
}

function totalValue(
  row: Overview['daily_usage'][number],
  metric: TrendMetric,
): number | null {
  switch (metric) {
    case 'requests':
      return row.requests
    case 'tokens':
      return row.tokens
    case 'cost':
      return row.cost_micros / 1_000_000
    case 'success':
      return row.requests > 0 ? row.success_rate : null
    case 'cache':
      return row.prompt_tokens > 0 ? row.cache_hit_rate : null
  }
}

function breakdownValue(
  row: DailyBreakdownUsage,
  metric: TrendMetric,
): number | null {
  switch (metric) {
    case 'requests':
      return row.requests
    case 'tokens':
      return row.tokens
    case 'cost':
      return row.cost_micros / 1_000_000
    case 'success':
      return row.requests > 0 ? row.success_rate : null
    case 'cache':
      return row.prompt_tokens > 0 ? row.cache_hit_rate : null
  }
}

// Expand the per-day breakdown into a dense day x series matrix so every line
// keeps a point for days it saw no traffic. Success rate skips empty days
// instead of drawing a misleading 0%.
function buildBreakdownSeries(
  breakdown: DailyBreakdownUsage[],
  days: Overview['daily_usage'],
  metric: TrendMetric,
): SeriesPoint[] {
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
    breakdown.map((row) => [
      `${row.day}\u0000${row.label}`,
      breakdownValue(row, metric),
    ]),
  )
  const points: SeriesPoint[] = []
  for (const day of days) {
    for (const label of labels) {
      const value = byDayLabel.get(`${day.day}\u0000${label}`)
      if ((metric === 'success' || metric === 'cache') && value == null) continue
      points.push({ day: day.day, label, value: value ?? 0 })
    }
  }
  return points
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
  const [view, setView] = useState<TrendView>('total')
  const [metric, setMetric] = useState<TrendMetric>('requests')
  const isBreakdown = view !== 'total'
  const points = useMemo(() => {
    if (!isBreakdown) {
      const series: SeriesPoint[] = []
      for (const row of data.daily_usage) {
        const value = totalValue(row, metric)
        if (value != null) {
          series.push({ day: row.day, label: '总体', value })
        }
      }
      return series
    }
    return buildBreakdownSeries(
      view === 'model' ? data.daily_model_requests : data.daily_provider_requests,
      data.daily_usage,
      metric,
    )
  }, [
    data.daily_model_requests,
    data.daily_provider_requests,
    data.daily_usage,
    isBreakdown,
    metric,
    view,
  ])
  const metricStyle = METRIC_STYLE[metric]
  const breakdownLabels = useMemo(
    () => (isBreakdown ? [...new Set(points.map((point) => point.label))] : []),
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
  const breakdownStroke = (group: SeriesPoint[]) =>
    breakdownColorMap[group[0]?.label] ?? BREAKDOWN_COLORS[0]
  const breakdownFill = (group: SeriesPoint[]) => {
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
          <div className="trend-toolbar">
            <div className="trend-metric-control">
              <Typography.Text type="secondary">指标</Typography.Text>
              <Segmented
                size="small"
                value={metric}
                onChange={(value) => setMetric(value as TrendMetric)}
                options={METRIC_OPTIONS}
              />
            </div>
            <div className="trend-split-control">
              <Typography.Text type="secondary">拆分</Typography.Text>
              <Select
                size="small"
                value={view}
                onChange={(value) => setView(value as TrendView)}
                options={VIEW_OPTIONS}
                style={{ minWidth: 108 }}
                popupMatchSelectWidth={false}
              />
            </div>
          </div>
          <div className="trend-chart">
            {points.length ? (
              <Line
                data={points}
                xField="day"
                yField="value"
                colorField={isBreakdown ? 'label' : undefined}
                seriesField={isBreakdown ? 'label' : undefined}
                height={260}
                axis={{
                  x: {
                    title: false,
                    tickCount: data.granularity === 'hour' ? 8 : 6,
                    labelAutoRotate: false,
                    labelAutoHide: true,
                    labelFormatter: (value: string) => {
                      switch (data.granularity) {
                        case 'hour':
                          return value.slice(11, 16)
                        case 'week':
                          return `${value.slice(5)} 周`
                        case 'month':
                          return value.slice(0, 7)
                        default:
                          return value.slice(5)
                      }
                    },
                  },
                  y: {
                    title: false,
                    labelFormatter: (value: number) =>
                      metric === 'success' || metric === 'cache'
                        ? `${value}%`
                        : metric === 'cost'
                          ? `$${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`
                          : formatCompact(value),
                  },
                }}
                scale={isBreakdown ? breakdownColorScale : undefined}
                legend={isBreakdown ? { color: { position: 'bottom' } } : undefined}
                style={
                  isBreakdown
                    ? { lineWidth: 2, stroke: breakdownStroke }
                    : { lineWidth: 2, stroke: metricStyle.color }
                }
                area={
                  isBreakdown
                    ? {
                        style: {
                          fill: breakdownFill,
                          fillOpacity: 1,
                        },
                      }
                    : {
                        style: {
                          fill: `linear-gradient(-90deg, ${withAlpha(metricStyle.color, 0.34)} 0%, ${withAlpha(metricStyle.color, 0.02)} 100%)`,
                        },
                      }
                }
                tooltip={{
                  items: [
                    {
                      channel: 'y',
                      name: metricStyle.name,
                      valueFormatter: metricStyle.format,
                    },
                  ],
                }}
              />
            ) : (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description="暂无请求数据"
              />
            )}
          </div>
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
            emptyText="暂无提供商用量"
          />
        </Card>
      </Col>
    </Row>
  )
}
