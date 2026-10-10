import { useEffect, useState } from 'react'
import { ReloadOutlined } from '@ant-design/icons'
import { App, Button, Card, Input, Select, Space, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { api, formatError } from '../api'
import PageHeader from '../components/PageHeader'

type AuditLog = {
  id: number
  created_at: string
  action: string
  entity: string
  entity_id?: string | null
  summary: string
  detail?: Record<string, unknown> | null
  actor: string
}

type AuditLogPage = {
  items: AuditLog[]
  total: number
  page: number
  page_size: number
}

const entityLabels: Record<string, string> = {
  provider: '提供商',
  route: '路由',
  api_key: '访问密钥',
  webhook: 'Webhook',
  settings: '设置',
  database: '数据库',
  usage: '请求日志',
}

const actionLabels: Record<string, string> = {
  create: '创建',
  update: '更新',
  delete: '删除',
  rotate: '轮换',
  action: '操作',
}

const actionColors: Record<string, string> = {
  create: 'success',
  update: 'processing',
  delete: 'error',
  rotate: 'warning',
  action: 'default',
}

export default function AuditLogsPage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<AuditLog[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(50)
  const [entity, setEntity] = useState<string>()
  const [action, setAction] = useState<string>()
  const [search, setSearch] = useState('')
  const [loading, setLoading] = useState(true)

  const load = async () => {
    setLoading(true)
    try {
      const params = new URLSearchParams({
        page: String(page),
        page_size: String(pageSize),
      })
      if (entity) params.set('entity', entity)
      if (action) params.set('action', action)
      if (search.trim()) params.set('search', search.trim())
      const result = await api.get<AuditLogPage>(`/api/audit-logs?${params.toString()}`)
      setItems(result.items)
      setTotal(result.total)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [page, pageSize, entity, action, search])

  return (
    <>
      <PageHeader
        title="审计日志"
        description="配置变更历史，记录每次创建、更新、删除和操作"
        extra={
          <Button icon={<ReloadOutlined />} loading={loading} onClick={() => void load()}>
            刷新
          </Button>
        }
      />
      <Card bordered={false}>
        <Space wrap style={{ marginBottom: 16 }}>
          <Select
            allowClear
            placeholder="对象"
            style={{ width: 150 }}
            value={entity}
            onChange={(value) => {
              setEntity(value)
              setPage(1)
            }}
            options={Object.entries(entityLabels).map(([value, label]) => ({ value, label }))}
          />
          <Select
            allowClear
            placeholder="操作"
            style={{ width: 130 }}
            value={action}
            onChange={(value) => {
              setAction(value)
              setPage(1)
            }}
            options={Object.entries(actionLabels).map(([value, label]) => ({ value, label }))}
          />
          <Input.Search
            allowClear
            placeholder="搜索摘要或 ID"
            style={{ width: 260 }}
            onSearch={(value) => {
              setSearch(value)
              setPage(1)
            }}
          />
        </Space>
        <Table<AuditLog>
          rowKey="id"
          size="small"
          loading={loading}
          dataSource={items}
          pagination={{
            current: page,
            pageSize,
            total,
            showSizeChanger: true,
            onChange: (nextPage, nextPageSize) => {
              setPage(nextPage)
              setPageSize(nextPageSize)
            },
          }}
          columns={[
            {
              title: '时间',
              dataIndex: 'created_at',
              width: 175,
              render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
            },
            {
              title: '操作',
              dataIndex: 'action',
              width: 90,
              render: (value: string) => (
                <Tag color={actionColors[value] || 'default'}>{actionLabels[value] || value}</Tag>
              ),
            },
            {
              title: '对象',
              dataIndex: 'entity',
              width: 110,
              render: (value: string) => entityLabels[value] || value,
            },
            {
              title: '摘要',
              dataIndex: 'summary',
              render: (value: string) => <Typography.Text>{value}</Typography.Text>,
            },
            {
              title: '对象 ID',
              dataIndex: 'entity_id',
              width: 110,
              render: (value?: string | null) => value || '-',
            },
            {
              title: '操作者',
              dataIndex: 'actor',
              width: 90,
              render: (value: string) => (
                <Tag color={value === 'admin' ? 'blue' : 'default'}>
                  {value === 'admin' ? '管理令牌' : '本地'}
                </Tag>
              ),
            },
            {
              title: '详情',
              dataIndex: 'detail',
              width: 90,
              render: (value?: Record<string, unknown> | null) =>
                value ? (
                  <Tooltip title={<pre className="code-block">{JSON.stringify(value, null, 2)}</pre>}>
                    <Typography.Link>查看</Typography.Link>
                  </Tooltip>
                ) : (
                  '-'
                ),
            },
          ]}
        />
      </Card>
    </>
  )
}
