import { CalendarOutlined, ProfileOutlined } from '@ant-design/icons'
import {
  Card,
  Empty,
  Segmented,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import type { Overview, UsageLog } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'
import AnalyticsErrors from './AnalyticsErrors'

type HeatmapMetric = 'requests' | 'tokens' | 'cost'

const METRIC_OPTIONS = [
  { label: '请求', value: 'requests' },
  { label: 'Token', value: 'tokens' },
  { label: '费用', value: 'cost' },
]

const WEEKDAYS = ['一', '', '三', '', '五', '', '日']

function metricValue(
  row: Overview['daily_usage'][number],
  metric: HeatmapMetric,
) {
  switch (metric) {
    case 'tokens':
      return row.tokens
    case 'cost':
      return row.cost_micros / 1_000_000
    default:
      return row.requests
  }
}

function formatMetric(value: number, metric: HeatmapMetric) {
  if (metric === 'cost') return formatCostMicros(Math.round(value * 1_000_000))
  return formatExact(Math.round(value))
}

function levelFor(value: number, max: number) {
  if (value <= 0 || max <= 0) return 0
  return Math.max(1, Math.min(4, Math.ceil((value / max) * 4)))
}

function requestTone(statusCode: number, success: boolean) {
  if (success) return 'success'
  return statusCode >= 500 ? 'error' : 'warning'
}

export default function AnalyticsRequests({
  data,
  dates,
}: {
  data: Overview
  dates: [Dayjs, Dayjs]
}) {
  const navigate = useNavigate()
  const [metric, setMetric] = useState<HeatmapMetric>('requests')
  const heatmapAvailable =
    data.granularity === 'day' || data.granularity === 'hour'

  const heatmap = useMemo(() => {
    const byDay = new Map<string, number>()
    for (const row of data.daily_usage) {
      const day = row.day.slice(0, 10)
      byDay.set(day, (byDay.get(day) ?? 0) + metricValue(row, metric))
    }
    const first = dates[0].startOf('day')
    const last = dates[1].startOf('day')
    const alignedStart = first.subtract((first.day() + 6) % 7, 'day')
    const cells: Array<{
      day: string
      value: number
      inRange: boolean
      level: number
    }> = []
    const rawCells: Array<{
      day: string
      value: number
      inRange: boolean
    }> = []
    let cursor = alignedStart
    while (cursor.isBefore(last) || cursor.isSame(last, 'day')) {
      const day = cursor.format('YYYY-MM-DD')
      rawCells.push({
        day,
        value: byDay.get(day) ?? 0,
        inRange: !cursor.isBefore(first, 'day') && !cursor.isAfter(last, 'day'),
      })
      cursor = cursor.add(1, 'day')
    }
    const max = Math.max(...rawCells.map((cell) => cell.value), 0)
    for (const cell of rawCells) {
      cells.push({ ...cell, level: levelFor(cell.value, max) })
    }
    return cells
  }, [data.daily_usage, dates, metric])

  const openDay = (day: string) => {
    const params = new URLSearchParams({
      from: dayjs(day).startOf('day').toISOString(),
      to: dayjs(day).add(1, 'day').startOf('day').toISOString(),
    })
    navigate(`/usage?${params}`)
  }

  const columns = [
    {
      title: '时间',
      dataIndex: 'created_at',
      width: 150,
      render: (value: string) => dayjs(value).format('MM-DD HH:mm:ss'),
    },
    {
      title: '状态',
      dataIndex: 'status_code',
      width: 85,
      render: (value: number, record: UsageLog) => (
        <Tag color={requestTone(value, record.success)}>{value}</Tag>
      ),
    },
    {
      title: '模型',
      dataIndex: 'requested_model',
      render: (value: string) => (
        <Typography.Text code ellipsis={{ tooltip: value }}>
          {value}
        </Typography.Text>
      ),
    },
    {
      title: '接口',
      dataIndex: 'endpoint',
      render: (value: string) => (
        <Typography.Text type="secondary" ellipsis={{ tooltip: value }}>
          {value}
        </Typography.Text>
      ),
    },
    {
      title: 'Token',
      dataIndex: 'total_tokens',
      width: 95,
      render: (value: number) => formatCompact(value),
    },
    {
      title: '费用',
      dataIndex: 'estimated_cost_micros',
      width: 100,
      render: (value: number | null) => formatCostMicros(value),
    },
    {
      title: '用时',
      dataIndex: 'latency_ms',
      width: 95,
      render: (value: number) => `${value} ms`,
    },
  ]

  return (
    <>
      <Card
        bordered={false}
        className="section-row"
        title={
          <Space size={8}>
            <CalendarOutlined />
            <span>请求日历</span>
          </Space>
        }
        extra={
          <Segmented
            size="small"
            value={metric}
            options={METRIC_OPTIONS}
            onChange={(value) => setMetric(value as HeatmapMetric)}
          />
        }
      >
        {heatmapAvailable ? (
          <>
            <div className="heatmap-range">
              <Typography.Text type="secondary">
                {dates[0].format('YYYY-MM-DD')} 至 {dates[1].format('YYYY-MM-DD')}
              </Typography.Text>
              <Typography.Text type="secondary">
                {data.range_requests.toLocaleString()} 次请求
              </Typography.Text>
            </div>
            <div className="heatmap-scroll">
              <div className="heatmap-layout">
                <div className="heatmap-weekdays" aria-hidden="true">
                  {WEEKDAYS.map((label, index) => (
                    <span key={`${label}-${index}`}>{label}</span>
                  ))}
                </div>
                <div className="heatmap-grid" role="grid" aria-label="每日请求用量">
                  {heatmap.map((cell) => (
                    <Tooltip
                      key={cell.day}
                      title={`${cell.day} · ${formatMetric(cell.value, metric)}`}
                    >
                      <button
                        type="button"
                        role="gridcell"
                        aria-label={`${cell.day} ${formatMetric(cell.value, metric)}`}
                        className={`heatmap-cell heatmap-level-${cell.level}${
                          cell.inRange ? '' : ' heatmap-cell-outside'
                        }`}
                        disabled={!cell.inRange}
                        onClick={() => openDay(cell.day)}
                      />
                    </Tooltip>
                  ))}
                </div>
              </div>
            </div>
            <div className="heatmap-legend">
              <span>少</span>
              {[0, 1, 2, 3, 4].map((level) => (
                <span
                  key={level}
                  className={`heatmap-cell heatmap-level-${level}`}
                />
              ))}
              <span>多</span>
            </div>
          </>
        ) : (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="切换粒度为自动或日以查看每日热力图"
          />
        )}
      </Card>

      <Card
        bordered={false}
        className="section-row"
        title={
          <Space size={8}>
            <ProfileOutlined />
            <span>最近请求</span>
          </Space>
        }
        extra={<Typography.Text type="secondary">{data.recent_requests.length} 条</Typography.Text>}
      >
        <Table
          rowKey="request_id"
          size="small"
          pagination={false}
          dataSource={data.recent_requests}
          locale={{ emptyText: '当前范围暂无请求' }}
          scroll={{ x: 920 }}
          onRow={(record) => ({
            onClick: () =>
              navigate(`/usage?request_id=${encodeURIComponent(record.request_id)}`),
            style: { cursor: 'pointer' },
          })}
          columns={columns}
        />
      </Card>
      <AnalyticsErrors data={data} />
    </>
  )
}
