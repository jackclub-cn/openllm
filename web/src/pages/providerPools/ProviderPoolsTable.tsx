import { DeleteOutlined, EditOutlined } from '@ant-design/icons'
import {
  Button,
  Card,
  Popconfirm,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import type { PoolUsage, ProviderPool } from '../../api'
import { formatCompact, formatExact } from '../../format'

type Props = {
  items: ProviderPool[]
  loading: boolean
  togglingId?: number
  usageByPool: Map<number, PoolUsage>
  onToggle: (pool: ProviderPool, enabled: boolean) => void
  onEdit: (pool: ProviderPool) => void
  onRemove: (id: number) => void
}

function formatWindow(seconds: number) {
  if (seconds < 60) return `${seconds} 秒`
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`
  return `${Math.floor(seconds / 3600)} 小时`
}

function usageCell(used: number, limit?: number | null) {
  return (
    <Typography.Text>
      {formatCompact(used)}
      <Typography.Text type="secondary">
        {' / '}
        {limit == null ? '不限' : formatCompact(limit)}
      </Typography.Text>
    </Typography.Text>
  )
}

/// The live pool state rides inside the request-usage cell: a separate status
/// column pushed the allocation column off the visible width, and these two
/// values are read together anyway.
function saturationTags(usage?: PoolUsage) {
  if (!usage) return null
  return (
    <Space size={4} wrap>
      <Tag color={usage.saturated ? 'error' : 'success'}>
        {usage.saturated ? '已饱和' : '宽松'}
      </Tag>
      {usage.denials > 0 && (
        <Tooltip title="本进程启动以来因超出份额被拒绝的次数">
          <Tag color="warning">拒绝 {usage.denials}</Tag>
        </Tooltip>
      )}
    </Space>
  )
}

export default function ProviderPoolsTable({
  items,
  loading,
  togglingId,
  usageByPool,
  onToggle,
  onEdit,
  onRemove,
}: Props) {
  return (
    <Card bordered={false}>
      <Table<ProviderPool>
        rowKey="id"
        loading={loading}
        dataSource={items}
        pagination={{ pageSize: 20, showSizeChanger: false }}
        scroll={{ x: 1040 }}
        locale={{ emptyText: '尚未配置配额池' }}
        columns={[
          {
            title: '状态',
            dataIndex: 'enabled',
            width: 78,
            render: (enabled: boolean, record) => (
              <Switch
                checked={enabled}
                loading={togglingId === record.id}
                onChange={(checked) => onToggle(record, checked)}
              />
            ),
          },
          {
            title: '名称',
            dataIndex: 'name',
            width: 164,
            render: (value: string, record) => (
              <Space direction="vertical" size={0}>
                <Typography.Text strong>{value}</Typography.Text>
                <Typography.Text type="secondary">
                  阈值 {record.saturation_threshold_pct}%
                </Typography.Text>
              </Space>
            ),
          },
          {
            title: '提供商',
            dataIndex: 'provider_id',
            width: 148,
            render: (value: number, record) =>
              record.provider_name || `#${value}`,
          },
          {
            title: '窗口',
            dataIndex: 'window_seconds',
            width: 92,
            render: (value: number) => formatWindow(value),
          },
          {
            title: '请求用量 / 上限',
            width: 176,
            render: (_, record) => {
              const usage = usageByPool.get(record.id)
              if (!usage) {
                return <Typography.Text type="secondary">—</Typography.Text>
              }
              return (
                <Space direction="vertical" size={2}>
                  {usageCell(usage.requests, usage.request_limit)}
                  {saturationTags(usage)}
                </Space>
              )
            },
          },
          {
            title: 'Token 用量 / 上限',
            width: 164,
            render: (_, record) => {
              const usage = usageByPool.get(record.id)
              return usage ? (
                usageCell(usage.tokens, usage.token_limit)
              ) : (
                <Typography.Text type="secondary">—</Typography.Text>
              )
            },
          },
          {
            title: '分配',
            width: 104,
            render: (_, record) => {
              const usage = usageByPool.get(record.id)
              const count = usage?.allocations.length ?? record.allocations.length
              return count > 0 ? (
                <Typography.Text>{count} 个密钥</Typography.Text>
              ) : (
                <Typography.Text type="secondary">未分配</Typography.Text>
              )
            },
          },
          {
            title: '操作',
            width: 104,
            fixed: 'right',
            render: (_, record) => (
              <Space size={4}>
                <Tooltip title="编辑">
                  <Button
                    type="text"
                    aria-label="编辑配额池"
                    icon={<EditOutlined />}
                    onClick={() => onEdit(record)}
                  />
                </Tooltip>
                <Popconfirm
                  title="删除此配额池？"
                  onConfirm={() => onRemove(record.id)}
                >
                  <Tooltip title="删除">
                    <Button
                      type="text"
                      danger
                      aria-label="删除配额池"
                      icon={<DeleteOutlined />}
                    />
                  </Tooltip>
                </Popconfirm>
              </Space>
            ),
          },
        ]}
        expandable={{
          rowExpandable: (pool) => pool.allocations.length > 0,
          expandedRowRender: (pool) => (
            <Table
              size="small"
              rowKey={(row) => `alloc-${row.id}`}
              pagination={false}
              dataSource={pool.allocations}
              columns={[
                {
                  title: '网关密钥',
                  render: (_, row) => row.api_key_name || `#${row.api_key_id}`,
                },
                {
                  title: '权重',
                  dataIndex: 'weight_pct',
                  render: (value: number) => `${value}%`,
                },
                { title: '策略', dataIndex: 'policy' },
                {
                  title: '请求份额',
                  render: (_, row) =>
                    row.request_share == null
                      ? '不限'
                      : formatExact(row.request_share),
                },
                {
                  title: 'Token 份额',
                  render: (_, row) =>
                    row.token_share == null ? '不限' : formatExact(row.token_share),
                },
              ]}
            />
          ),
        }}
      />
    </Card>
  )
}
