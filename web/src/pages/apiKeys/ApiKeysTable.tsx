import {
  DeleteOutlined,
  EditOutlined,
  HistoryOutlined,
  KeyOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Button, Card, Popconfirm, Space, Switch, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { useNavigate } from 'react-router-dom'
import type { ApiKey } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type ApiKeysTableProps = {
  items: ApiKey[]
  itemCount: number
  loading: boolean
  page: number
  onPageChange: (page: number) => void
  onToggle: (id: number, enabled: boolean) => void
  onEditLimits: (record: ApiKey) => void
  onRotate: (id: number) => void
  onRemove: (id: number) => void
}

export default function ApiKeysTable({
  items,
  itemCount,
  loading,
  page,
  onPageChange,
  onToggle,
  onEditLimits,
  onRotate,
  onRemove,
}: ApiKeysTableProps) {
  const navigate = useNavigate()

  return (
    <Card bordered={false}>
      <Table
        rowKey="id"
        loading={loading}
        dataSource={items}
        pagination={{
          current: page,
          defaultPageSize: 20,
          pageSizeOptions: [10, 20, 50, 100],
          showSizeChanger: true,
          showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
          onChange: onPageChange,
        }}
        locale={{
          emptyText: itemCount ? '没有符合筛选条件的密钥' : '暂无访问密钥',
        }}
        scroll={{ x: 1920 }}
        columns={[
          {
            title: '名称',
            dataIndex: 'name',
            render: (value: string) => (
              <Space>
                <KeyOutlined />
                {value}
              </Space>
            ),
          },
          {
            title: '密钥',
            width: 220,
            render: (_, record) => (
              <Tooltip title={`${record.key_prefix}...${record.key_suffix}`}>
                <Typography.Text code className="api-key-value">
                  {record.key_prefix}...{record.key_suffix}
                </Typography.Text>
              </Tooltip>
            ),
          },
          {
            title: '最后使用',
            dataIndex: 'last_used_at',
            width: 170,
            render: (value?: string) =>
              value ? dayjs(value).format('YYYY-MM-DD HH:mm:ss') : '从未使用',
          },
          {
            title: '请求',
            dataIndex: 'requests',
            width: 80,
          },
          {
            title: '输入 tokens',
            dataIndex: 'prompt_tokens',
            width: 120,
            render: (value: number) => (
              <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
            ),
          },
          {
            title: '输出 tokens',
            dataIndex: 'completion_tokens',
            width: 120,
            render: (value: number) => (
              <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
            ),
          },
          {
            title: '费用',
            dataIndex: 'cost_micros',
            width: 120,
            render: (value: number | null, record) => (
              <div>
                <div>{formatCostMicros(value)}</div>
                {record.unpriced_requests > 0 && (
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {record.unpriced_requests} 条未定价
                  </Typography.Text>
                )}
              </div>
            ),
          },
          {
            title: '今日用量 / 限额',
            width: 270,
            render: (_, record) => {
              const limits: string[] = []
              if (record.today_requests > 0) {
                limits.push(`请求 ${record.today_requests}`)
              }
              if (record.daily_token_limit != null) {
                limits.push(
                  `总 token ${formatCompact(record.today_tokens)} / ${formatCompact(record.daily_token_limit)} · 输入 ${formatCompact(record.today_prompt_tokens)} / 输出 ${formatCompact(record.today_completion_tokens)}`,
                )
              } else if (record.today_tokens > 0) {
                limits.push(
                  `总 token ${formatCompact(record.today_tokens)} · 输入 ${formatCompact(record.today_prompt_tokens)} / 输出 ${formatCompact(record.today_completion_tokens)}`,
                )
              }
              if (record.daily_cost_limit_micros != null) {
                limits.push(
                  `${formatCostMicros(record.today_cost_micros)} / ${formatCostMicros(record.daily_cost_limit_micros)}`,
                )
              } else if (record.today_cost_micros != null) {
                limits.push(formatCostMicros(record.today_cost_micros))
              }
              return limits.length ? (
                <Space direction="vertical" size={0}>
                  {limits.map((limit) => (
                    <Typography.Text key={limit}>{limit}</Typography.Text>
                  ))}
                </Space>
              ) : (
                <Typography.Text type="secondary">今日无消耗</Typography.Text>
              )
            },
          },
          {
            title: '速率限制',
            width: 180,
            render: (_, record) => {
              const limits: string[] = []
              if (record.requests_per_minute != null) {
                limits.push(
                  `${record.requests_this_minute} / ${record.requests_per_minute} 次/分钟`,
                )
              }
              if (record.max_concurrency != null) {
                limits.push(`并发 ${record.current_in_flight} / ${record.max_concurrency}`)
              } else if (record.current_in_flight > 0) {
                limits.push(`并发 ${record.current_in_flight}`)
              }
              return limits.length ? (
                <Space direction="vertical" size={0}>
                  {limits.map((limit) => (
                    <Typography.Text key={limit}>{limit}</Typography.Text>
                  ))}
                </Space>
              ) : (
                <Typography.Text type="secondary">不限</Typography.Text>
              )
            },
          },
          {
            title: '模型权限',
            width: 110,
            render: (_, record) =>
              record.allowed_models.length ? (
                <Tooltip title={record.allowed_models.join('\n')}>
                  <Tag color="blue">{record.allowed_models.length} 条规则</Tag>
                </Tooltip>
              ) : (
                <Tag>全部模型</Tag>
              ),
          },
          {
            title: '到期时间',
            dataIndex: 'expires_at',
            width: 150,
            render: (value?: string | null) =>
              value ? dayjs(value).format('YYYY-MM-DD HH:mm') : '永不过期',
          },
          {
            title: '创建时间',
            dataIndex: 'created_at',
            width: 150,
            render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm'),
          },
          {
            title: '状态',
            dataIndex: 'enabled',
            width: 130,
            render: (value: boolean, record) => {
              const expired = record.expires_at
                ? dayjs(record.expires_at).isBefore(dayjs())
                : false
              return (
                <Space size={8}>
                  <Switch
                    size="small"
                    checked={value}
                    onChange={(checked) => onToggle(record.id, checked)}
                  />
                  <Tag color={expired ? 'error' : value ? 'success' : 'default'}>
                    {expired ? '已过期' : value ? '启用' : '停用'}
                  </Tag>
                </Space>
              )
            },
          },
          {
            title: '操作',
            width: 190,
            render: (_, record) => (
              <Space>
                <Tooltip title="查看请求日志">
                  <Button
                    type="text"
                    icon={<HistoryOutlined />}
                    onClick={() => navigate(`/usage?api_key_id=${record.id}`)}
                  />
                </Tooltip>
                <Tooltip title="编辑限额">
                  <Button
                    type="text"
                    icon={<EditOutlined />}
                    onClick={() => onEditLimits(record)}
                  />
                </Tooltip>
                <Popconfirm
                  title="轮换此密钥？旧密钥会立即失效。"
                  onConfirm={() => onRotate(record.id)}
                >
                  <Tooltip title="轮换密钥">
                    <Button type="text" icon={<ReloadOutlined />} />
                  </Tooltip>
                </Popconfirm>
                <Popconfirm title="删除此密钥？" onConfirm={() => onRemove(record.id)}>
                  <Button type="text" danger icon={<DeleteOutlined />} />
                </Popconfirm>
              </Space>
            ),
          },
        ]}
      />
    </Card>
  )
}
