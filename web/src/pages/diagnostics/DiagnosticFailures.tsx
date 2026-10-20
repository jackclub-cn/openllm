import { Card, Space, Statistic, Table, Tag, Typography } from 'antd'
import { type DiagnosticFailure, type DiagnosticSnapshot } from '../../api'
import { formatTimestamp } from './format'

type Props = { snapshot?: DiagnosticSnapshot; loading: boolean }
export default function DiagnosticFailures({ snapshot, loading }: Props) {
  return (
    <Card title="最近失败请求">
      <Space size={16} style={{ marginBottom: 12 }} wrap>
        <Statistic title="24 小时请求" value={snapshot?.requests_24h.total ?? 0} />
        <Statistic title="成功" value={snapshot?.requests_24h.successful ?? 0} />
        <Statistic
          title="失败"
          value={snapshot?.requests_24h.failed ?? 0}
          valueStyle={
            (snapshot?.requests_24h.failed ?? 0) > 0 ? { color: '#d4380d' } : undefined
          }
        />
        <div>
          <Typography.Text type="secondary">失败状态码</Typography.Text>
          <div>
            {(snapshot?.requests_24h.failures_by_status || []).map((item) => (
              <Tag key={item.status_code} color="error">
                {item.status_code}：{item.count}
              </Tag>
            ))}
          </div>
        </div>
      </Space>
      <Table<DiagnosticFailure>
        size="small"
        scroll={{ x: 720 }}
        rowKey="request_id"
        loading={loading}
        pagination={false}
        dataSource={snapshot?.recent_failures || []}
        locale={{ emptyText: '没有失败记录' }}
        columns={[
          {
            title: '时间',
            dataIndex: 'created_at',
            width: 170,
            render: (value: string) => formatTimestamp(value),
          },
          { title: '模型', dataIndex: 'upstream_model', width: 140 },
          { title: '状态码', dataIndex: 'status_code', width: 80 },
          {
            title: '延迟',
            dataIndex: 'latency_ms',
            width: 90,
            render: (value: number) => `${value} ms`,
          },
          {
            title: '错误',
            dataIndex: 'error_message',
            render: (value: string | null, row) => (
              <Typography.Text type="secondary" ellipsis={{ tooltip: true }}>
                {value || row.warning_message || '—'}
              </Typography.Text>
            ),
          },
        ]}
      />
      <Typography.Paragraph type="secondary" style={{ marginTop: 8, marginBottom: 0 }}>
        快照与页面只包含错误摘要，不包含提示词或模型回复内容。
      </Typography.Paragraph>
    </Card>
  )
}
