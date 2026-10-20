import { Card, Table, Tag, Typography } from 'antd'
import { type DiagnosticSnapshot, type PoolUsage } from '../../api'
import { formatSeconds } from './format'

type Props = {
  snapshot?: DiagnosticSnapshot
  loading: boolean
  keyNames: Map<number, string>
}
export default function DiagnosticPools({ snapshot, loading, keyNames }: Props) {
  return (
    <Card title="提供商配额池">
      <Table<PoolUsage>
        size="small"
        scroll={{ x: 720 }}
        rowKey="pool_id"
        pagination={false}
        loading={loading}
        dataSource={snapshot?.pools || []}
        locale={{ emptyText: '尚未配置配额池' }}
        expandable={{
          rowExpandable: (pool) => pool.allocations.length > 0,
          expandedRowRender: (pool) => (
            <Table
              size="small"
              scroll={{ x: 560 }}
              rowKey={(row) => `alloc-${row.api_key_id}`}
              pagination={false}
              dataSource={pool.allocations}
              columns={[
                {
                  title: '网关密钥',
                  dataIndex: 'api_key_id',
                  render: (value: number) => keyNames.get(value) || `#${value}`,
                },
                {
                  title: '权重',
                  dataIndex: 'weight_pct',
                  render: (value: number) => `${value}%`,
                },
                { title: '策略', dataIndex: 'policy' },
                {
                  title: '本窗口请求 / 份额',
                  render: (_: unknown, row) =>
                    `${row.key_requests} / ${row.request_share ?? '不限'}`,
                },
                {
                  title: '本窗口 Token / 份额',
                  render: (_: unknown, row) =>
                    `${row.key_tokens} / ${row.token_share ?? '不限'}`,
                },
              ]}
            />
          ),
        }}
        columns={[
          { title: '配额池', dataIndex: 'name' },
          {
            title: '提供商',
            dataIndex: 'provider_id',
            render: (value: number) =>
              snapshot?.providers.find((item) => item.id === value)?.name || `#${value}`,
          },
          {
            title: '窗口',
            dataIndex: 'window_seconds',
            render: (value: number) => formatSeconds(value),
          },
          {
            title: '请求用量 / 上限',
            render: (_: unknown, row) =>
              `${row.requests} / ${row.request_limit ?? '不限'}`,
          },
          {
            title: 'Token 用量 / 上限',
            render: (_: unknown, row) => `${row.tokens} / ${row.token_limit ?? '不限'}`,
          },
          {
            title: '饱和度',
            dataIndex: 'saturated',
            width: 90,
            render: (value: boolean) =>
              value ? <Tag color="warning">已饱和</Tag> : <Tag color="success">正常</Tag>,
          },
          {
            title: '拒绝',
            dataIndex: 'denials',
            width: 80,
            render: (value: number) =>
              value > 0 ? <Typography.Text type="danger">{value}</Typography.Text> : 0,
          },
          {
            title: '分配',
            width: 70,
            render: (_: unknown, row) => row.allocations.length,
          },
        ]}
      />
    </Card>
  )
}
