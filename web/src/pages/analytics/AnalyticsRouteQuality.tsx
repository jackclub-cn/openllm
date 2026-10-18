import { useCallback, useEffect, useState } from 'react'
import { ReloadOutlined } from '@ant-design/icons'
import {
  App,
  Button,
  Card,
  Col,
  Empty,
  Progress,
  Row,
  Statistic,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import type { TableColumnsType } from 'antd'
import type { Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type AnalyticsGranularity,
  type RouteQuality,
  type RouteQualityDecision,
  type RouteQualityFailure,
  type RouteQualityRoute,
  type RouteQualityStrategy,
} from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'
import { buildAnalyticsParams } from './queryParams'
import {
  routeStrategyLabel,
  sessionDecisionLabel,
  traceReasonLabel,
} from './routingLabels'

type AnalyticsRouteQualityProps = {
  dates: [Dayjs, Dayjs]
  filters: UsageDimensionFilterValues
  granularity: AnalyticsGranularity
}

function percentTag(value: number) {
  return (
    <Tag color={value <= 5 ? 'success' : value <= 20 ? 'warning' : 'error'}>
      {value.toFixed(1)}%
    </Tag>
  )
}

function successTag(value: number) {
  return (
    <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
      {value.toFixed(1)}%
    </Tag>
  )
}

export default function AnalyticsRouteQuality({
  dates,
  filters,
  granularity,
}: AnalyticsRouteQualityProps) {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [data, setData] = useState<RouteQuality>()
  const [loading, setLoading] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const params = buildAnalyticsParams(dates, filters, granularity)
      setData(
        await api.get<RouteQuality>(
          `/api/analytics/route-quality?${params}`,
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

  const strategyColumns: TableColumnsType<RouteQualityStrategy> = [
    {
      title: '策略',
      dataIndex: 'strategy',
      render: (value: string) => (
        <Typography.Text strong>{routeStrategyLabel(value)}</Typography.Text>
      ),
    },
    {
      title: '轨迹数',
      dataIndex: 'traces',
      width: 100,
      sorter: (left, right) => left.traces - right.traces,
    },
    {
      title: '回退率',
      dataIndex: 'fallback_rate',
      width: 100,
      sorter: (left, right) => left.fallback_rate - right.fallback_rate,
      render: percentTag,
    },
    {
      title: '平均尝试',
      dataIndex: 'avg_attempts',
      width: 110,
      sorter: (left, right) => left.avg_attempts - right.avg_attempts,
      render: (value: number) => value.toFixed(2),
    },
    {
      title: '成功率',
      dataIndex: 'success_rate',
      width: 100,
      sorter: (left, right) => left.success_rate - right.success_rate,
      render: successTag,
    },
  ]

  const routeColumns: TableColumnsType<RouteQualityRoute> = [
    {
      title: '路由',
      dataIndex: 'route_name',
      render: (value: string) => (
        <Typography.Text strong ellipsis={{ tooltip: value }}>
          {value}
        </Typography.Text>
      ),
    },
    {
      title: '轨迹数',
      dataIndex: 'traces',
      width: 100,
      sorter: (left, right) => left.traces - right.traces,
    },
    {
      title: '回退率',
      dataIndex: 'fallback_rate',
      width: 100,
      sorter: (left, right) => left.fallback_rate - right.fallback_rate,
      render: percentTag,
    },
    {
      title: '平均尝试',
      dataIndex: 'avg_attempts',
      width: 110,
      sorter: (left, right) => left.avg_attempts - right.avg_attempts,
      render: (value: number) => value.toFixed(2),
    },
    {
      title: '成功率',
      dataIndex: 'success_rate',
      width: 100,
      sorter: (left, right) => left.success_rate - right.success_rate,
      render: successTag,
    },
  ]

  const decisionColumns: TableColumnsType<RouteQualityDecision> = [
    {
      title: '会话决策',
      dataIndex: 'decision',
      render: (value: string) => sessionDecisionLabel(value),
    },
    {
      title: '轨迹数',
      dataIndex: 'count',
      width: 110,
      sorter: (left, right) => left.count - right.count,
    },
    {
      title: '占比',
      width: 180,
      render: (_, record) => {
        const percent =
          data && data.session_requests > 0
            ? (record.count / data.session_requests) * 100
            : 0
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
  ]

  const failureColumns: TableColumnsType<RouteQualityFailure> = [
    {
      title: '失败原因',
      dataIndex: 'reason_code',
      render: (value: string) => traceReasonLabel(value),
    },
    {
      title: '故障域',
      dataIndex: 'failure_scope',
      width: 120,
      render: (value: string) => <Tag>{value}</Tag>,
    },
    {
      title: '次数',
      dataIndex: 'count',
      width: 100,
      sorter: (left, right) => left.count - right.count,
    },
  ]

  return (
    <Card
      title="路由质量"
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
      <Row gutter={[16, 16]}>
        <Col xs={12} sm={8} xl={4}>
          <Statistic title="路由轨迹" value={data?.traces ?? 0} />
        </Col>
        <Col xs={12} sm={8} xl={5}>
          <Statistic
            title="回退率"
            value={data?.fallback_rate ?? 0}
            precision={1}
            suffix="%"
          />
        </Col>
        <Col xs={12} sm={8} xl={5}>
          <Statistic
            title="平均尝试次数"
            value={data?.avg_attempts ?? 0}
            precision={2}
          />
        </Col>
        <Col xs={12} sm={8} xl={5}>
          <Tooltip
            title={`${data?.session_hits ?? 0} / ${data?.session_requests ?? 0} 条会话请求命中已有粘性目标`}
          >
            <Statistic
              title="粘性命中率"
              value={data?.session_hit_rate ?? 0}
              precision={1}
              suffix="%"
            />
          </Tooltip>
        </Col>
        <Col xs={12} sm={8} xl={5}>
          <Statistic
            title="成功率"
            value={data?.success_rate ?? 0}
            precision={1}
            suffix="%"
          />
        </Col>
      </Row>

      <Typography.Title level={5} style={{ marginTop: 24 }}>
        策略表现
      </Typography.Title>
      <Table
        rowKey="strategy"
        size="small"
        loading={loading}
        pagination={false}
        dataSource={data?.strategies ?? []}
        locale={{
          emptyText: (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="当前筛选下暂无路由轨迹"
            />
          ),
        }}
        scroll={{ x: 680 }}
        columns={strategyColumns}
      />

      <Row gutter={[24, 24]} style={{ marginTop: 24 }}>
        <Col xs={24} xl={12}>
          <Typography.Title level={5}>路由表现</Typography.Title>
          <Table
            rowKey={(record) => String(record.route_id ?? record.route_name)}
            size="small"
            loading={loading}
            pagination={{ pageSize: 8, hideOnSinglePage: true }}
            dataSource={data?.routes ?? []}
            onRow={(record) => ({
              onClick: () => {
                if (record.route_id) navigate(`/usage?route_id=${record.route_id}`)
              },
              style: { cursor: record.route_id ? 'pointer' : undefined },
            })}
            locale={{
              emptyText: (
                <Empty
                  image={Empty.PRESENTED_IMAGE_SIMPLE}
                  description="暂无路由数据"
                />
              ),
            }}
            scroll={{ x: 680 }}
            columns={routeColumns}
          />
        </Col>
        <Col xs={24} xl={12}>
          <Typography.Title level={5}>会话粘性</Typography.Title>
          <Table
            rowKey="decision"
            size="small"
            loading={loading}
            pagination={false}
            dataSource={data?.session_decisions ?? []}
            locale={{
              emptyText: (
                <Empty
                  image={Empty.PRESENTED_IMAGE_SIMPLE}
                  description="当前筛选下暂无会话请求"
                />
              ),
            }}
            scroll={{ x: 420 }}
            columns={decisionColumns}
          />
        </Col>
      </Row>

      <Typography.Title level={5} style={{ marginTop: 24 }}>
        失败原因
      </Typography.Title>
      <Table
        rowKey={(record) => `${record.reason_code}\u0000${record.failure_scope}`}
        size="small"
        loading={loading}
        pagination={{ pageSize: 10, hideOnSinglePage: true }}
        dataSource={data?.failure_reasons ?? []}
        locale={{
          emptyText: (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="当前筛选下暂无失败尝试"
            />
          ),
        }}
        scroll={{ x: 520 }}
        columns={failureColumns}
      />
    </Card>
  )
}
