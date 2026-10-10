import {
  DeleteOutlined,
  EditOutlined,
  ExperimentOutlined,
  HistoryOutlined,
} from '@ant-design/icons'
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
import dayjs from 'dayjs'
import type { Webhook } from '../../api'

type Props = {
  items: Webhook[]
  loading: boolean
  togglingId?: number
  testingId?: number
  onToggle: (webhook: Webhook, enabled: boolean) => void
  onEdit: (webhook: Webhook) => void
  onTest: (webhook: Webhook) => void
  onDeliveries: (webhook: Webhook) => void
  onRemove: (id: number) => void
}

export default function WebhooksTable({
  items,
  loading,
  togglingId,
  testingId,
  onToggle,
  onEdit,
  onTest,
  onDeliveries,
  onRemove,
}: Props) {
  return (
    <Card bordered={false}>
      <Table<Webhook>
        rowKey="id"
        loading={loading}
        dataSource={items}
        pagination={{ pageSize: 20, showSizeChanger: false }}
        scroll={{ x: 980 }}
        columns={[
          {
            title: '状态',
            dataIndex: 'enabled',
            width: 88,
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
            width: 180,
            render: (value: string, record) => (
              <Space direction="vertical" size={0}>
                <Typography.Text strong>{value}</Typography.Text>
                <Typography.Text type={record.secret_set ? 'success' : 'secondary'}>
                  {record.secret_set ? '已签名' : '未签名'}
                </Typography.Text>
              </Space>
            ),
          },
          {
            title: '投递地址',
            dataIndex: 'url',
            ellipsis: true,
            render: (value: string) => (
              <Typography.Text copyable={{ text: value }}>{value}</Typography.Text>
            ),
          },
          {
            title: '订阅事件',
            dataIndex: 'event_types',
            width: 210,
            render: (events: string[]) => (
              <Space wrap size={4}>
                {events.map((event) => (
                  <Tag key={event} color={event === 'request.failed' ? 'error' : 'success'}>
                    {event === 'request.failed' ? '失败' : '成功'}
                  </Tag>
                ))}
              </Space>
            ),
          },
          {
            title: '最后投递',
            width: 180,
            render: (_, record) =>
              record.last_delivery_at ? (
                <Space direction="vertical" size={0}>
                  <Typography.Text>
                    {dayjs(record.last_delivery_at).format('MM-DD HH:mm:ss')}
                  </Typography.Text>
                  <Tag
                    color={
                      record.last_delivery_status &&
                      record.last_delivery_status >= 200 &&
                      record.last_delivery_status < 300
                        ? 'success'
                        : 'error'
                    }
                  >
                    {record.last_delivery_status || '连接失败'}
                  </Tag>
                </Space>
              ) : (
                <Typography.Text type="secondary">暂无记录</Typography.Text>
              ),
          },
          {
            title: '近期失败',
            dataIndex: 'recent_failures',
            width: 96,
            render: (value: number) => (
              <Tag color={value ? 'error' : 'default'}>{value}</Tag>
            ),
          },
          {
            title: '操作',
            width: 188,
            fixed: 'right',
            render: (_, record) => (
              <Space size={4}>
                <Tooltip title="发送测试事件">
                  <Button
                    type="text"
                    aria-label="测试 Webhook"
                    icon={<ExperimentOutlined />}
                    loading={testingId === record.id}
                    onClick={() => onTest(record)}
                  />
                </Tooltip>
                <Tooltip title="投递记录">
                  <Button
                    type="text"
                    aria-label="查看投递记录"
                    icon={<HistoryOutlined />}
                    onClick={() => onDeliveries(record)}
                  />
                </Tooltip>
                <Tooltip title="编辑">
                  <Button
                    type="text"
                    aria-label="编辑 Webhook"
                    icon={<EditOutlined />}
                    onClick={() => onEdit(record)}
                  />
                </Tooltip>
                <Popconfirm title="删除此 Webhook？" onConfirm={() => onRemove(record.id)}>
                  <Tooltip title="删除">
                    <Button
                      type="text"
                      danger
                      aria-label="删除 Webhook"
                      icon={<DeleteOutlined />}
                    />
                  </Tooltip>
                </Popconfirm>
              </Space>
            ),
          },
        ]}
      />
    </Card>
  )
}
