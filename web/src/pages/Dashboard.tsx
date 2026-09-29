import { useCallback, useEffect, useState } from 'react'
import {
  ApiOutlined,
  ClockCircleOutlined,
  DatabaseOutlined,
  NodeIndexOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import { Alert, Button, Card, Col, Empty, Progress, Row, Skeleton, Space, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { Area } from '@ant-design/plots'
import { api, formatError, type Overview } from '../api'
import PageHeader from '../components/PageHeader'
import MetricCard from '../components/MetricCard'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'

export default function Dashboard() {
  const [data, setData] = useState<Overview>()
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    try {
      // Let the server bucket "today" and the daily chart by the viewer's
      // timezone rather than UTC.
      const offset = -new Date().getTimezoneOffset()
      setData(await api.get<Overview>(`/api/overview?tz_offset_minutes=${offset}`))
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const autoRefresh = useAutoRefresh({
    intervalMs: 30_000,
    onRefresh: load,
  })
  const { connected: realtimeConnected } = useRealtime()
  useCoalescedUsageEvents(() => {
    void autoRefresh.manualRefresh()
  }, { enabled: !autoRefresh.paused })

  if (error) return <Alert type="error" showIcon message="无法加载仪表盘" description={error} />
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  return (
    <>
      <PageHeader
        title="运行概览"
        description="网关请求、模型令牌与提供商健康状态"
        extra={
          <>
            {autoRefresh.lastUpdated && (
              <Typography.Text type="secondary">
                更新于 {dayjs(autoRefresh.lastUpdated).format('HH:mm:ss')}
              </Typography.Text>
            )}
            <Tag color={realtimeConnected ? 'success' : 'default'}>
              {realtimeConnected ? '实时' : '轮询'}
            </Tag>
            <Tooltip title={autoRefresh.paused ? '恢复自动刷新' : '暂停自动刷新'}>
              <Button
                icon={autoRefresh.paused ? <PlayCircleOutlined /> : <PauseCircleOutlined />}
                onClick={() => autoRefresh.setPaused((value) => !value)}
              >
                {autoRefresh.paused ? '已暂停' : '自动刷新'}
              </Button>
            </Tooltip>
            <Button
              icon={<ReloadOutlined spin={autoRefresh.refreshing} />}
              loading={autoRefresh.refreshing}
              onClick={() => void autoRefresh.manualRefresh()}
            >
              刷新
            </Button>
          </>
        }
      />
      <Row gutter={[16, 16]}>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard label="今日请求" value={data.requests_today} icon={<ThunderboltOutlined />} tone="blue" />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard label="今日令牌" value={data.tokens_today} icon={<ApiOutlined />} tone="cyan" />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard label="成功率" value={data.success_rate} precision={1} suffix="%" icon={<NodeIndexOutlined />} tone="green" />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="平均延迟"
            value={data.avg_latency_ms}
            precision={0}
            suffix="ms"
            icon={<ClockCircleOutlined />}
            tone="orange"
          />
        </Col>
      </Row>
      <Row gutter={[16, 16]} className="section-row">
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="缓存命中率"
            value={data.cache_hit_rate}
            precision={1}
            suffix="%"
            icon={<DatabaseOutlined />}
            tone="purple"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日缓存读取"
            value={data.cache_read_today}
            icon={<DatabaseOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日缓存写入"
            value={data.cache_write_today}
            icon={<DatabaseOutlined />}
            tone="cyan"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计缓存读取"
            value={data.cache_read_total}
            icon={<DatabaseOutlined />}
            tone="blue"
          />
        </Col>
      </Row>

      <Row gutter={[16, 16]} className="section-row">
        <Col xs={24} xl={15}>
          <Card title="近 14 天请求量" bordered={false}>
            {data.daily_usage.length ? (
              <Area
                data={data.daily_usage}
                xField="day"
                yField="requests"
                height={260}
                axis={{ x: { title: false }, y: { title: false } }}
                style={{ fill: 'linear-gradient(-90deg, white 0%, #1677ff 100%)' }}
                line={{ style: { stroke: '#1677ff', strokeWidth: 2 } }}
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
                columns={[
                  {
                    title: '提供商',
                    dataIndex: 'provider_name',
                    ellipsis: true,
                  },
                  { title: '请求', dataIndex: 'requests', width: 70 },
                  {
                    title: '成功率',
                    dataIndex: 'success_rate',
                    width: 90,
                    render: (value: number, record) =>
                      record.requests === 0 ? (
                        <Typography.Text type="secondary">无调用</Typography.Text>
                      ) : (
                        <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
                          {value.toFixed(1)}%
                        </Tag>
                      ),
                  },
                  {
                    title: '延迟',
                    dataIndex: 'avg_latency_ms',
                    width: 90,
                    render: (value: number, record) =>
                      record.requests === 0 ? '-' : `${Math.round(value)} ms`,
                  },
                ]}
              />
            ) : (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="请先添加上游提供商" />
            )}
          </Card>
        </Col>
      </Row>

      <Row gutter={[16, 16]} className="section-row">
        <Col xs={24} xl={16}>
          <Card title="最近请求" bordered={false}>
            <Table
              rowKey="id"
              size="middle"
              pagination={false}
              dataSource={data.recent_requests}
              locale={{ emptyText: '暂无请求' }}
              columns={[
                {
                  title: '模型',
                  dataIndex: 'requested_model',
                  render: (value: string, record) => (
                    <div>
                      <Typography.Text strong>{value}</Typography.Text>
                      {record.upstream_model && (
                        <div><Typography.Text type="secondary">{record.upstream_model}</Typography.Text></div>
                      )}
                    </div>
                  ),
                },
                {
                  title: '状态',
                  dataIndex: 'success',
                  width: 92,
                  render: (value: boolean, record) => (
                    <Tag color={value ? 'success' : 'error'}>{value ? '成功' : record.status_code}</Tag>
                  ),
                },
                { title: '令牌', dataIndex: 'total_tokens', width: 90 },
                {
                  title: '延迟',
                  dataIndex: 'latency_ms',
                  width: 100,
                  render: (value: number) => `${value} ms`,
                },
                {
                  title: '时间',
                  dataIndex: 'created_at',
                  width: 170,
                  render: (value: string) => dayjs(value).format('MM-DD HH:mm:ss'),
                },
              ]}
            />
          </Card>
        </Col>
        <Col xs={24} xl={8}>
          <Card title="网关配置" bordered={false} className="full-height-card">
            <Space direction="vertical" size={24} style={{ width: '100%' }}>
              <div>
                <Typography.Text type="secondary">活跃提供商</Typography.Text>
                <div className="stat-line"><strong>{data.active_providers}</strong> 个</div>
              </div>
              <div>
                <Typography.Text type="secondary">活跃路由</Typography.Text>
                <div className="stat-line"><strong>{data.active_routes}</strong> 条</div>
              </div>
              <div>
                <Typography.Text type="secondary">累计请求</Typography.Text>
                <div className="stat-line"><strong>{data.requests_total}</strong> 次</div>
              </div>
              <div>
                <Typography.Text type="secondary">累计令牌</Typography.Text>
                <div className="stat-line"><strong>{data.tokens_total}</strong></div>
              </div>
              <Progress percent={Math.round(data.success_rate)} status="active" strokeColor="#52c41a" />
            </Space>
          </Card>
        </Col>
      </Row>

      <Card title="模型用量（近 14 天）" bordered={false} className="section-row">
        <Table
          rowKey="model"
          size="middle"
          pagination={false}
          dataSource={data.model_usage}
          locale={{ emptyText: '暂无模型用量' }}
          scroll={{ x: 720 }}
          columns={[
            {
              title: '模型',
              dataIndex: 'model',
              render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
            },
            { title: '请求数', dataIndex: 'requests', width: 100 },
            {
              title: '令牌',
              dataIndex: 'tokens',
              width: 120,
              sorter: (a, b) => a.tokens - b.tokens,
            },
            {
              title: '成功率',
              dataIndex: 'success_rate',
              width: 160,
              render: (value: number) => (
                <Progress
                  percent={Math.round(value)}
                  size="small"
                  status={value >= 99 ? 'success' : value >= 90 ? 'normal' : 'exception'}
                />
              ),
            },
            {
              title: '平均延迟',
              dataIndex: 'avg_latency_ms',
              width: 120,
              sorter: (a, b) => a.avg_latency_ms - b.avg_latency_ms,
              render: (value: number) => `${Math.round(value)} ms`,
            },
          ]}
        />
      </Card>
    </>
  )
}
