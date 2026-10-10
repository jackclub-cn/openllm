import { DeleteOutlined, EditOutlined, HistoryOutlined } from '@ant-design/icons'
import { Button, Card, Popconfirm, Space, Switch, Table, Tag, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { GatewayRoute } from '../../api'
import { formatCompact, formatExact } from '../../format'
import { strategyLabels } from './types'

type RoutesTableProps = {
  routes: GatewayRoute[]
  routeCount: number
  loading: boolean
  page: number
  togglingId?: number
  onPageChange: (page: number) => void
  onToggleEnabled: (record: GatewayRoute, enabled: boolean) => void
  onEdit: (record: GatewayRoute) => void
  onRemove: (id: number) => void
}

export default function RoutesTable({
  routes,
  routeCount,
  loading,
  page,
  togglingId,
  onPageChange,
  onToggleEnabled,
  onEdit,
  onRemove,
}: RoutesTableProps) {
  const navigate = useNavigate()

  return (
    <Card bordered={false}>
      <Table
        rowKey="id"
        loading={loading}
        dataSource={routes}
        pagination={{
          current: page,
          defaultPageSize: 20,
          pageSizeOptions: [10, 20, 50, 100],
          showSizeChanger: true,
          showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
          onChange: onPageChange,
        }}
        locale={{ emptyText: routeCount ? '没有符合筛选条件的路由' : '暂无路由' }}
        scroll={{ x: 1100 }}
        columns={[
          {
            title: '路由',
            dataIndex: 'name',
            render: (value: string, record) => (
              <div>
                <Typography.Text strong>{value}</Typography.Text>
                <div>
                  <Typography.Text code>{record.model_pattern}</Typography.Text>
                </div>
              </div>
            ),
          },
          {
            title: '策略',
            dataIndex: 'strategy',
            width: 140,
            render: (value: GatewayRoute['strategy']) => (
              <Tag color="blue">{strategyLabels[value]}</Tag>
            ),
          },
          {
            title: '能力桶',
            width: 190,
            render: (_, record) => {
              const limits = [
                record.input_limit != null ? `输入 ${formatCompact(record.input_limit)}` : '',
                record.output_limit != null ? `输出 ${formatCompact(record.output_limit)}` : '',
              ].filter(Boolean)
              return (
                <div>
                  {limits.length ? (
                    <Tooltip
                      title={[
                        record.context_limit != null
                          ? `公共上下文 ${formatExact(record.context_limit)}`
                          : '',
                        record.input_limit != null
                          ? `公共输入 ${formatExact(record.input_limit)}`
                          : '',
                        record.output_limit != null
                          ? `公共输出 ${formatExact(record.output_limit)}`
                          : '',
                      ]
                        .filter(Boolean)
                        .join(' / ')}
                    >
                      <Space wrap size={[4, 4]}>
                        {limits.map((limit) => (
                          <Tag key={limit}>{limit}</Tag>
                        ))}
                      </Space>
                    </Tooltip>
                  ) : (
                    <Typography.Text type="secondary">未声明</Typography.Text>
                  )}
                  {!record.limits_verified && (
                    <div>
                      <Typography.Text type="warning" style={{ fontSize: 12 }}>
                        部分目标缺少能力元数据
                      </Typography.Text>
                    </div>
                  )}
                </div>
              )
            },
          },
          {
            title: '上游目标',
            dataIndex: 'targets',
            render: (targets: GatewayRoute['targets']) => (
              <Space wrap>
                {targets.map((target, index) => {
                  const endpoints = target.supported_endpoints?.map((endpoint) =>
                    endpoint.replace(/^\/v1/, ''),
                  )
                  const available =
                    target.enabled &&
                    target.provider_enabled !== false &&
                    target.model_enabled !== false
                  const unavailableReason = !target.enabled
                    ? '目标已停用'
                    : target.provider_enabled === false
                      ? '提供商已停用'
                      : target.model_enabled === false
                        ? '模型已停用'
                        : ''
                  return (
                    <Tooltip
                      key={`${target.id ?? index}-${target.provider_id}`}
                      title={[
                        unavailableReason,
                        endpoints?.length
                          ? `支持接口：${endpoints.join('、')}`
                          : '未声明接口限制，所有兼容接口均可尝试',
                      ]
                        .filter(Boolean)
                        .join(' · ')}
                    >
                      <Tag color={available ? 'cyan' : 'default'}>
                        {target.provider_name} / {target.model_prefix || ''}
                        {target.upstream_model}
                        {endpoints?.length ? ` · ${endpoints.join(', ')}` : ''}
                        {!available ? ' · 不可用' : ''}
                      </Tag>
                    </Tooltip>
                  )
                })}
              </Space>
            ),
          },
          {
            title: '状态',
            dataIndex: 'enabled',
            width: 100,
            render: (value: boolean, record) => (
              <Switch
                checked={value}
                loading={togglingId === record.id}
                checkedChildren="启用"
                unCheckedChildren="停用"
                onChange={(checked) => onToggleEnabled(record, checked)}
              />
            ),
          },
          {
            title: '操作',
            width: 150,
            fixed: 'right',
            render: (_, record) => (
              <Space>
                <Tooltip title="查看请求日志">
                  <Button
                    type="text"
                    icon={<HistoryOutlined />}
                    onClick={() => navigate(`/usage?route_id=${record.id}`)}
                  />
                </Tooltip>
                <Tooltip title="编辑">
                  <Button type="text" icon={<EditOutlined />} onClick={() => onEdit(record)} />
                </Tooltip>
                <Popconfirm title="删除此路由？" onConfirm={() => onRemove(record.id)}>
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
