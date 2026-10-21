import { Alert, Descriptions, Empty, Space, Spin, Table, Tag, Typography } from 'antd'
import type { TableColumnsType } from 'antd'
import type { RouteTrace, RouteTraceAttempt, RouteTraceCandidate } from '../../api'
import { traceReasonLabel } from '../analytics/routingLabels'

export default function RouteTracePanel({
  trace,
  loading,
  error,
}: {
  trace?: RouteTrace
  loading: boolean
  error?: string
}) {
  const candidateColumns: TableColumnsType<RouteTraceCandidate> = [
    { title: '顺序', dataIndex: 'order', width: 64 },
    {
      title: '候选',
      key: 'candidate',
      render: (_, item) => `#${item.provider_id} / ${item.upstream_model}`,
    },
    {
      title: '状态',
      key: 'state',
      width: 96,
      render: (_, item) =>
        item.deprioritized ? <Tag color="orange">降权</Tag> : <Tag color="green">候选</Tag>,
    },
    {
      title: '原因',
      dataIndex: 'reason_code',
      render: traceReasonLabel,
    },
  ]
  const attemptColumns: TableColumnsType<RouteTraceAttempt> = [
    { title: '序号', dataIndex: 'attempt_index', width: 64 },
    {
      title: '候选',
      key: 'candidate',
      render: (_, item) => `#${item.provider_id ?? '-'} / ${item.upstream_model || '-'}`,
    },
    {
      title: '结果',
      key: 'result',
      width: 110,
      render: (_, item) =>
        item.success ? (
          <Tag color="green">{item.status_code ?? '成功'}</Tag>
        ) : (
          <Tag color={item.retryable ? 'orange' : 'red'}>
            {item.status_code ?? traceReasonLabel(item.reason_code)}
          </Tag>
        ),
    },
    {
      title: '耗时',
      dataIndex: 'latency_ms',
      width: 88,
      render: (value: number) => `${value} ms`,
    },
    {
      title: '分类',
      key: 'failure',
      render: (_, item) =>
        item.success ? '-' : `${item.failure_scope || '-'} / ${traceReasonLabel(item.reason_code)}`,
    },
  ]

  if (loading) {
    return (
      <div style={{ padding: 32, textAlign: 'center' }}>
        <Spin />
      </div>
    )
  }
  if (error) {
    return <Alert type="info" showIcon message="暂无路由轨迹" description={error} />
  }
  if (!trace) return <Empty description="暂无路由轨迹" />

  return (
    <Space direction="vertical" size={16} style={{ width: '100%' }}>
      <Descriptions column={2} size="small" bordered>
        <Descriptions.Item label="策略">{trace.strategy}</Descriptions.Item>
        <Descriptions.Item label="尝试次数">{trace.attempt_count}</Descriptions.Item>
        <Descriptions.Item label="回退次数">{trace.fallback_count}</Descriptions.Item>
        <Descriptions.Item label="会话粘性">
          {trace.session_decision === 'no_affinity' ? '未启用' : '已识别会话'}
        </Descriptions.Item>
        <Descriptions.Item label="最终提供商">
          {trace.final_provider_id ? `#${trace.final_provider_id}` : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="最终模型">
          {trace.final_upstream_model || '-'}
        </Descriptions.Item>
      </Descriptions>
      <div>
        <Typography.Text strong>候选顺序</Typography.Text>
        <Table
          rowKey="order"
          size="small"
          pagination={false}
          columns={candidateColumns}
          dataSource={trace.candidates}
        />
      </div>
      <div>
        <Typography.Text strong>实际尝试</Typography.Text>
        {trace.attempts.length === 0 ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有上游尝试" />
        ) : (
          <Table
            rowKey="attempt_index"
            size="small"
            pagination={false}
            columns={attemptColumns}
            dataSource={trace.attempts}
          />
        )}
      </div>
    </Space>
  )
}
