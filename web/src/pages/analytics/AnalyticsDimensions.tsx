import { useCallback, useEffect, useMemo, useState } from 'react'
import { ReloadOutlined } from '@ant-design/icons'
import { App, Button, Card, Empty, Progress, Segmented, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type AnalyticsGranularity,
  type AnalyticsDimensions as AnalyticsDimensionsData,
  type DimensionUsage,
  type SessionUsage,
} from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'
import { formatCompact, formatCostMicros, formatExact } from '../../format'
import { buildAnalyticsParams } from './queryParams'

type DimensionKey = 'api_key' | 'provider_key' | 'route' | 'endpoint' | 'session'

const DIMENSION_OPTIONS = [
  { label: '访问', value: 'api_key' },
  { label: '上游', value: 'provider_key' },
  { label: '路由', value: 'route' },
  { label: '接口', value: 'endpoint' },
  { label: '会话', value: 'session' },
]

type AnalyticsDimensionsProps = {
  dates: [Dayjs, Dayjs]
  filters: UsageDimensionFilterValues
  granularity: AnalyticsGranularity
}

function requestShare(requests: number, total: number) {
  return total > 0 ? (requests / total) * 100 : 0
}

function successTag(value: number, requests: number) {
  if (requests === 0) return <Typography.Text type="secondary">无调用</Typography.Text>
  return (
    <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
      {value.toFixed(1)}%
    </Tag>
  )
}

export default function AnalyticsDimensions({
  dates,
  filters,
  granularity,
}: AnalyticsDimensionsProps) {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [dimension, setDimension] = useState<DimensionKey>('api_key')
  const [data, setData] = useState<AnalyticsDimensionsData>()
  const [loading, setLoading] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const params = buildAnalyticsParams(dates, filters, granularity)
      setData(
        await api.get<AnalyticsDimensionsData>(
          `/api/analytics/dimensions?${params}`,
        ),
      )
    } catch (reason) {
      message.error(formatError(reason))
    } finally {
      setLoading(false)
    }
  }, [dates, filters, granularity])

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void load()
    }, 250)
    return () => window.clearTimeout(timer)
  }, [load])

  const rows: DimensionUsage[] = useMemo(() => {
    if (!data) return []
    switch (dimension) {
      case 'api_key':
        return data.api_key_usage
      case 'provider_key':
        return data.provider_api_key_usage
      case 'route':
        return data.route_usage
      case 'endpoint':
        return data.endpoint_usage
      default:
        return []
    }
  }, [data, dimension])

  const totalRequests = rows.reduce((sum, row) => sum + row.requests, 0)

  const openUsage = (row: DimensionUsage) => {
    const params = new URLSearchParams()
    switch (dimension) {
      case 'api_key':
        if (row.id) params.set('api_key_id', String(row.id))
        break
      case 'provider_key':
        if (row.id) params.set('provider_api_key_id', String(row.id))
        break
      case 'route':
        if (row.id) params.set('route_id', String(row.id))
        break
      case 'endpoint':
        params.set('endpoint', row.label)
        break
    }
    navigate(`/usage?${params}`)
  }

  const sessionColumns = [
    {
      title: '会话',
      dataIndex: 'session_id',
      render: (value: string) => (
        <Typography.Text strong ellipsis={{ tooltip: value }} style={{ maxWidth: 260 }}>
          {value}
        </Typography.Text>
      ),
    },
    { title: '请求数', dataIndex: 'requests', width: 100 },
    {
      title: 'tokens',
      dataIndex: 'tokens',
      width: 110,
      render: (value: number, record: SessionUsage) => (
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
      width: 110,
      render: (value: number | null) => formatCostMicros(value),
    },
    {
      title: '缓存',
      dataIndex: 'cache_hit_rate',
      width: 110,
      render: (value: number, record: SessionUsage) => (
        <Tooltip
          title={`读取 ${formatExact(record.cache_read)} · 写入 ${formatExact(record.cache_write)}`}
        >
          <span>{value.toFixed(1)}%</span>
        </Tooltip>
      ),
    },
    {
      title: '成功率',
      dataIndex: 'success_rate',
      width: 110,
      render: (value: number, record: SessionUsage) =>
        successTag(value, record.requests),
    },
    {
      title: '平均总用时',
      dataIndex: 'avg_latency_ms',
      width: 130,
      render: (value: number) => `${Math.round(value)} ms`,
    },
    {
      title: '最近活动',
      dataIndex: 'last_seen',
      width: 170,
      render: (value: string) => dayjs(value).format('MM-DD HH:mm:ss'),
    },
  ]

  const dimensionColumns = [
    {
      title: '名称',
      dataIndex: 'label',
      render: (value: string) => (
        <Typography.Text strong ellipsis={{ tooltip: value }} style={{ maxWidth: 260 }}>
          {value}
        </Typography.Text>
      ),
    },
    {
      title: '请求数',
      dataIndex: 'requests',
      width: 100,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.requests - right.requests,
    },
    {
      title: '请求占比',
      width: 140,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.requests - right.requests,
      render: (_: unknown, record: DimensionUsage) => {
        const percent = requestShare(record.requests, totalRequests)
        return (
          <Tooltip title={`${percent.toFixed(1)}%`}>
            <Progress
              percent={Math.round(percent)}
              size="small"
              showInfo={false}
              strokeColor="#1677ff"
            />
          </Tooltip>
        )
      },
    },
    {
      title: 'tokens',
      dataIndex: 'tokens',
      width: 110,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.tokens - right.tokens,
      render: (value: number, record: DimensionUsage) => (
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
      width: 110,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        (left.cost_micros ?? 0) - (right.cost_micros ?? 0),
      render: (value: number | null) => formatCostMicros(value),
    },
    {
      title: '缓存',
      dataIndex: 'cache_hit_rate',
      width: 110,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.cache_hit_rate - right.cache_hit_rate,
      render: (value: number, record: DimensionUsage) => (
        <Tooltip
          title={`读取 ${formatExact(record.cache_read)} · 写入 ${formatExact(record.cache_write)}`}
        >
          <span>{value.toFixed(1)}%</span>
        </Tooltip>
      ),
    },
    {
      title: '成功率',
      dataIndex: 'success_rate',
      width: 110,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.success_rate - right.success_rate,
      render: (value: number, record: DimensionUsage) =>
        successTag(value, record.requests),
    },
    {
      title: '平均总用时',
      dataIndex: 'avg_latency_ms',
      width: 130,
      sorter: (left: DimensionUsage, right: DimensionUsage) =>
        left.avg_latency_ms - right.avg_latency_ms,
      render: (value: number) => `${Math.round(value)} ms`,
    },
  ]

  return (
    <Card
      title="分维度用量"
      bordered={false}
      className="section-row"
      extra={
        <Button
          size="small"
          icon={<ReloadOutlined />}
          loading={loading}
          onClick={() => void load()}
        >
          刷新
        </Button>
      }
    >
      <Segmented
        block
        size="small"
        value={dimension}
        onChange={(value) => setDimension(value as DimensionKey)}
        options={DIMENSION_OPTIONS}
        style={{ marginBottom: 16 }}
      />
      {dimension === 'session' ? (
        <Table
          rowKey="session_id"
          size="middle"
          loading={loading}
          pagination={{ pageSize: 10, hideOnSinglePage: true }}
          dataSource={data?.session_usage ?? []}
          onRow={(record) => ({
            onClick: () =>
              navigate(`/usage?session_id=${encodeURIComponent(record.session_id)}`),
            style: { cursor: 'pointer' },
          })}
          locale={{
            emptyText: (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="当前筛选下暂无会话" />
            ),
          }}
          scroll={{ x: 900 }}
          columns={sessionColumns}
        />
      ) : (
        <Table
          rowKey={(record) => record.id ?? record.label}
          size="middle"
          loading={loading}
          pagination={{ pageSize: 10, hideOnSinglePage: true }}
          dataSource={rows}
          onRow={(record) => ({
            onClick: () => openUsage(record),
            style: { cursor: 'pointer' },
          })}
          locale={{
            emptyText: (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="当前筛选下暂无数据" />
            ),
          }}
          scroll={{ x: 1000 }}
          columns={dimensionColumns}
        />
      )}
    </Card>
  )
}
