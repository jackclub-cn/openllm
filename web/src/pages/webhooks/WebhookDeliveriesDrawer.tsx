import { useEffect, useState } from 'react'
import { HistoryOutlined } from '@ant-design/icons'
import { App, Drawer, Space, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import {
  api,
  formatError,
  type Webhook,
  type WebhookDelivery,
} from '../../api'

type Props = {
  webhook?: Webhook
  onClose: () => void
}

export default function WebhookDeliveriesDrawer({ webhook, onClose }: Props) {
  const { message } = App.useApp()
  const [items, setItems] = useState<WebhookDelivery[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    if (!webhook) {
      setItems([])
      return
    }
    setLoading(true)
    api
      .get<WebhookDelivery[]>(`/api/webhooks/${webhook.id}/deliveries`)
      .then(setItems)
      .catch((error) => message.error(formatError(error)))
      .finally(() => setLoading(false))
  }, [webhook, message])

  return (
    <Drawer
      title={
        webhook && (
          <Space>
            <HistoryOutlined />
            {webhook.name} 投递记录
          </Space>
        )
      }
      open={Boolean(webhook)}
      onClose={onClose}
      width={820}
      destroyOnHidden
    >
      <Table<WebhookDelivery>
        rowKey="id"
        size="small"
        loading={loading}
        dataSource={items}
        pagination={{ pageSize: 20, showSizeChanger: false }}
        columns={[
          {
            title: '时间',
            dataIndex: 'created_at',
            width: 170,
            render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
          },
          {
            title: '事件',
            dataIndex: 'event_type',
            width: 150,
            render: (value: string) => (
              <Tag color={value === 'request.failed' ? 'error' : 'success'}>
                {value === 'request.failed' ? '请求失败' : '请求成功'}
              </Tag>
            ),
          },
          {
            title: '请求 ID',
            dataIndex: 'request_id',
            width: 190,
            render: (value?: string | null) =>
              value ? (
                <Typography.Text copyable={{ text: value }}>{value}</Typography.Text>
              ) : (
                <Typography.Text type="secondary">测试投递</Typography.Text>
              ),
          },
          {
            title: '结果',
            width: 110,
            render: (_, record) =>
              record.status_code && record.status_code >= 200 && record.status_code < 300 ? (
                <Tag color="success">{record.status_code}</Tag>
              ) : record.status_code ? (
                <Tag color="error">{record.status_code}</Tag>
              ) : (
                <Tag color="error">连接失败</Tag>
              ),
          },
          {
            title: '尝试',
            dataIndex: 'attempts',
            width: 70,
          },
          {
            title: '耗时',
            dataIndex: 'duration_ms',
            width: 90,
            render: (value: number) => `${value} ms`,
          },
          {
            title: '错误',
            dataIndex: 'error',
            ellipsis: true,
            render: (value?: string | null) =>
              value ? (
                <Tooltip title={value}>
                  <Typography.Text type="danger">{value}</Typography.Text>
                </Tooltip>
              ) : (
                '-'
              ),
          },
        ]}
      />
    </Drawer>
  )
}
