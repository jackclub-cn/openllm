import { WarningOutlined } from '@ant-design/icons'
import { Button, Card, Space, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import type { UsageLog } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type UsageTableProps = {
  items: UsageLog[]
  loading: boolean
  page: number
  pageSize: number
  total: number
  onPageChange: (page: number, pageSize: number) => void
  onOpenDetail: (record: UsageLog) => void
}

export default function UsageTable({
  items,
  loading,
  page,
  pageSize,
  total,
  onPageChange,
  onOpenDetail,
}: UsageTableProps) {
  return (
    <Card bordered={false} className="table-card">
      <Table
        rowKey="id"
        loading={loading}
        dataSource={items}
        scroll={{ x: 2000 }}
        onRow={(record) => ({
          onClick: () => onOpenDetail(record),
          style: { cursor: 'pointer' },
        })}
        pagination={{
          current: page,
          pageSize,
          total,
          showSizeChanger: true,
          showTotal: (value) => `共 ${value} 条`,
          onChange: onPageChange,
        }}
        columns={[
          {
            title: '时间',
            dataIndex: 'created_at',
            width: 170,
            render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
          },
          {
            title: '模型',
            dataIndex: 'requested_model',
            width: 170,
            render: (value: string, record) => (
              <div>
                <Typography.Text strong>{value}</Typography.Text>
                {record.upstream_model && (
                  <div>
                    <Typography.Text type="secondary">{record.upstream_model}</Typography.Text>
                  </div>
                )}
              </div>
            ),
          },
          {
            title: '提供商 / 路由',
            width: 170,
            render: (_, record) => (
              <div>
                <Space size={4}>
                  <span>
                    {record.provider_name ||
                      (record.provider_id ? `#${record.provider_id}` : '-')}
                  </span>
                  {record.provider_api_key_name && (
                    <Tooltip title={`上游密钥 #${record.provider_api_key_id ?? '-'}`}>
                      <Tag color="blue">{record.provider_api_key_name}</Tag>
                    </Tooltip>
                  )}
                </Space>
                <Typography.Text type="secondary">
                  {record.route_name || (record.route_id ? `#${record.route_id}` : '自动路由')}
                </Typography.Text>
              </div>
            ),
          },
          {
            title: '访问密钥',
            dataIndex: 'api_key_name',
            width: 140,
            render: (value: string | undefined, record) =>
              value ? (
                <Tag color="geekblue">{value}</Tag>
              ) : (
                <Typography.Text type="secondary">
                  {record.api_key_id ? `#${record.api_key_id}` : '匿名调用'}
                </Typography.Text>
              ),
          },
          {
            title: '接口',
            dataIndex: 'endpoint',
            width: 180,
            render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
          },
          {
            title: '状态',
            dataIndex: 'success',
            width: 100,
            render: (value: boolean, record) => (
              <Space size={4}>
                {record.in_flight ? (
                  <Tag color="processing">请求中</Tag>
                ) : (
                  <Tag color={value ? 'success' : 'error'}>
                    {value ? '成功' : record.status_code}
                  </Tag>
                )}
                {record.warning_message && (
                  <Tooltip title={record.warning_message}>
                    <WarningOutlined style={{ color: '#d48806' }} />
                  </Tooltip>
                )}
              </Space>
            ),
          },
          {
            title: 'tokens',
            key: 'tokens',
            width: 150,
            render: (_, record) => {
              if (record.in_flight) {
                return <Typography.Text type="secondary">-</Typography.Text>
              }
              const cached =
                (record.cache_read_tokens || 0) + (record.cache_write_tokens || 0)
              return (
                <div>
                  <Tooltip
                    title={`输入 ${formatExact(record.prompt_tokens)} / 输出 ${formatExact(record.completion_tokens)} / 总 tokens ${formatExact(record.total_tokens)}`}
                  >
                    <div>
                      <Typography.Text type="secondary">输入 </Typography.Text>
                      {formatCompact(record.prompt_tokens)}
                    </div>
                    <div>
                      <Typography.Text type="secondary">输出 </Typography.Text>
                      {formatCompact(record.completion_tokens)}
                    </div>
                  </Tooltip>
                  {cached > 0 && (
                    <div>
                      <Tooltip
                        title={`缓存读取 ${formatExact(record.cache_read_tokens)} / 缓存写入 ${formatExact(record.cache_write_tokens)}`}
                      >
                        <Typography.Text type="success" style={{ fontSize: 12 }}>
                          缓存 {formatCompact(record.cache_read_tokens)}
                        </Typography.Text>
                      </Tooltip>
                    </div>
                  )}
                </div>
              )
            },
          },
          {
            title: '总用时',
            dataIndex: 'latency_ms',
            width: 100,
            render: (value: number, record) =>
              record.in_flight ? (
                <Typography.Text type="secondary">-</Typography.Text>
              ) : (
                `${value} ms`
              ),
          },
          {
            title: '费用',
            dataIndex: 'estimated_cost_micros',
            width: 90,
            render: (value: number | null, record) =>
              record.in_flight ? (
                <Typography.Text type="secondary">-</Typography.Text>
              ) : (
                formatCostMicros(value)
              ),
          },
          {
            title: '首 token 用时',
            dataIndex: 'first_token_ms',
            width: 130,
            render: (value: number | undefined, record) =>
              record.in_flight ? (
                <Typography.Text type="secondary">-</Typography.Text>
              ) : value != null ? (
                <Tooltip
                  title={
                    record.streamed
                      ? '从请求开始到收到首个输出片段'
                      : '标准响应没有增量时间戳，按完整响应总用时记录'
                  }
                >
                  <span>{value} ms</span>
                </Tooltip>
              ) : (
                <Typography.Text type="secondary">不适用</Typography.Text>
              ),
          },
          {
            title: 'TPS',
            dataIndex: 'output_tps',
            width: 100,
            render: (value: number | undefined, record) => (
              <Tooltip
                title={
                  record.streamed
                    ? '生成阶段速度（已排除首 token 等待）'
                    : '含等待时间的整体速度'
                }
              >
                <span>
                  {record.in_flight || value == null ? '-' : `${value.toFixed(1)} tok/s`}
                </span>
              </Tooltip>
            ),
          },
          {
            title: '类型',
            dataIndex: 'streamed',
            width: 90,
            render: (value: boolean) => <Tag>{value ? '流式' : '标准'}</Tag>,
          },
          {
            title: '请求 ID',
            dataIndex: 'request_id',
            width: 170,
            render: (value: string) => (
              <Typography.Text copyable={{ text: value }}>
                {value.slice(0, 12)}…
              </Typography.Text>
            ),
          },
          {
            title: '错误',
            dataIndex: 'error_message',
            width: 150,
            render: (value: string | undefined, record) =>
              value ? (
                <Tooltip title={value}>
                  <Button
                    type="link"
                    size="small"
                    danger
                    onClick={(event) => {
                      event.stopPropagation()
                      onOpenDetail(record)
                    }}
                  >
                    查看错误
                  </Button>
                </Tooltip>
              ) : (
                '-'
              ),
          },
        ]}
      />
    </Card>
  )
}
