import { useCallback, useEffect, useRef, useState } from 'react'
import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  CopyOutlined,
  DownloadOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  DatePicker,
  Descriptions,
  Drawer,
  Input,
  InputNumber,
  Modal,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import type { Dayjs } from 'dayjs'
import dayjs from 'dayjs'
import { api, formatError, type Provider, type UsageLog } from '../api'
import { formatCompact, formatCostMicros, formatExact } from '../format'
import PageHeader from '../components/PageHeader'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'

type UsagePageResponse = {
  items: UsageLog[]
  total: number
  page: number
  page_size: number
}

export default function Usage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<UsageLog[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [total, setTotal] = useState(0)
  const [model, setModel] = useState('')
  const [requestId, setRequestId] = useState('')
  const [providerId, setProviderId] = useState<number>()
  const [success, setSuccess] = useState<boolean>()
  const [dates, setDates] = useState<[Dayjs, Dayjs]>()
  const [newRequests, setNewRequests] = useState(0)
  const [autoScroll, setAutoScroll] = useState(true)
  const [detail, setDetail] = useState<UsageLog>()
  const [detailLoading, setDetailLoading] = useState(false)
  const [cleanupOpen, setCleanupOpen] = useState(false)
  const [cleanupDays, setCleanupDays] = useState<number | null>(30)
  const [cleaning, setCleaning] = useState(false)
  const [exporting, setExporting] = useState(false)
  const known = useRef<{ filter: string; total: number } | undefined>(undefined)
  const queryRef = useRef({ page, pageSize, model, providerId, success, dates, autoScroll })
  queryRef.current = { page, pageSize, model, providerId, success, dates, autoScroll }
  const filterKey = JSON.stringify([model, requestId, providerId, success, dates?.[0]?.toISOString(), dates?.[1]?.toISOString()])

  useEffect(() => {
    // A different filter set produces a different total, so any pending
    // baseline would manufacture phantom "new requests".
    setNewRequests(0)
  }, [filterKey])

  const load = useCallback(async (
    nextPage = page,
    nextPageSize = pageSize,
    mode: 'manual' | 'auto' = 'manual',
  ) => {
    if (mode === 'manual') setLoading(true)
    const params = new URLSearchParams({
      page: String(nextPage),
      page_size: String(nextPageSize),
    })
    if (model) params.set('model', model)
    if (requestId) params.set('request_id', requestId)
    if (providerId) params.set('provider_id', String(providerId))
    if (success !== undefined) params.set('success', String(success))
    if (dates?.[0]) params.set('from', dates[0].startOf('day').toISOString())
    if (dates?.[1]) params.set('to', dates[1].endOf('day').toISOString())
    try {
      const result = await api.get<UsagePageResponse>(`/api/usage?${params}`)
      const currentFilter = JSON.stringify([
        model,
        requestId,
        providerId,
        success,
        dates?.[0]?.toISOString(),
        dates?.[1]?.toISOString(),
      ])
      const previous = known.current
      const sameFilter = previous?.filter === currentFilter
      if (mode === 'auto') {
        // Compare the filtered row count instead of the current page's max id:
        // on any page other than the first, new rows never appear in the
        // fetched window, so an id-based check silently misses them.
        // Only rows added under the *same* filter are genuine new requests.
        const added = sameFilter && previous
          ? Math.max(0, result.total - previous.total)
          : 0
        if (added > 0) {
          if (autoScroll && nextPage === 1) {
            setNewRequests(0)
          } else {
            // Freeze the visible rows and surface a banner instead of
            // swapping content out from under the user.
            setNewRequests((value) => value + added)
            known.current = { filter: currentFilter, total: result.total }
            return
          }
        }
      }
      known.current = { filter: currentFilter, total: result.total }
      setItems(result.items)
      setTotal(result.total)
      setPage(result.page)
      setPageSize(result.page_size)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      if (mode === 'manual') setLoading(false)
    }
  }, [autoScroll, dates, model, page, pageSize, providerId, requestId, success])

  const loadFromRef = useCallback(async (mode: 'manual' | 'auto' = 'auto') => {
    const current = queryRef.current
    await load(current.page, current.pageSize, mode)
  }, [load])

  useEffect(() => {
    void Promise.all([
      api.get<Provider[]>('/api/providers').then(setProviders),
      load(1, 20),
    ]).catch((error) => message.error(formatError(error)))
  }, [])

  const autoRefresh = useAutoRefresh({
    intervalMs: 15_000,
    onRefresh: () => loadFromRef('auto'),
  })
  const { connected: realtimeConnected } = useRealtime()
  useCoalescedUsageEvents(() => {
    void loadFromRef('auto')
  }, { enabled: !autoRefresh.paused })

  const showLatest = async () => {
    setNewRequests(0)
    await load(1, pageSize)
  }

  const openDetail = async (record: UsageLog) => {
    setDetail(record)
    setDetailLoading(true)
    try {
      setDetail(await api.get<UsageLog>(`/api/usage/${record.request_id}`))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setDetailLoading(false)
    }
  }

  const runCleanup = async () => {
    if (!cleanupDays || cleanupDays < 1) {
      message.warning('保留天数至少为 1')
      return
    }
    setCleaning(true)
    try {
      const result = await api.post<{ deleted: number; older_than_days: number }>(
        '/api/usage/cleanup',
        { older_than_days: cleanupDays },
      )
      message.success(`已清理 ${result.deleted} 条早于 ${result.older_than_days} 天的记录`)
      setCleanupOpen(false)
      await load(1, pageSize)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setCleaning(false)
    }
  }

  const exportCsv = async () => {
    setExporting(true)
    try {
      const params = new URLSearchParams()
      if (model) params.set('model', model)
      if (requestId) params.set('request_id', requestId)
      if (providerId) params.set('provider_id', String(providerId))
      if (success !== undefined) params.set('success', String(success))
      if (dates?.[0]) params.set('from', dates[0].startOf('day').toISOString())
      if (dates?.[1]) params.set('to', dates[1].endOf('day').toISOString())
      const blob = await api.download(`/api/usage/export?${params}`)
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = `openllm-usage-${dayjs().format('YYYYMMDD-HHmmss')}.csv`
      link.click()
      URL.revokeObjectURL(url)
      message.success('CSV 已导出')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setExporting(false)
    }
  }

  return (
    <>
      <PageHeader
        title="请求日志"
        description="查询每一次模型调用、令牌用量、延迟与错误"
        extra={
          <>
            {autoRefresh.lastUpdated && (
              <Typography.Text type="secondary">
                更新于 {dayjs(autoRefresh.lastUpdated).format('HH:mm:ss')}
              </Typography.Text>
            )}
            <Tag color={realtimeConnected ? 'success' : 'default'}>
              {realtimeConnected ? '实时' : '轮询'}
            </Tag>
            <Tooltip title="第一页时自动切换为最新记录">
              <Space size={6}>
                <Switch
                  size="small"
                  checked={autoScroll}
                  onChange={(value) => {
                    setAutoScroll(value)
                    if (value && page === 1) setNewRequests(0)
                  }}
                />
                <Typography.Text type="secondary">自动置顶</Typography.Text>
              </Space>
            </Tooltip>
            <Tooltip title={autoRefresh.paused ? '恢复自动刷新' : '暂停自动刷新'}>
              <Button
                icon={autoRefresh.paused ? <PlayCircleOutlined /> : <PauseCircleOutlined />}
                onClick={() => autoRefresh.setPaused((value) => !value)}
              >
                {autoRefresh.paused ? '已暂停' : '自动刷新'}
              </Button>
            </Tooltip>
            <Button
              icon={<ReloadOutlined spin={autoRefresh.refreshing} />}
              loading={autoRefresh.refreshing}
              onClick={() => void autoRefresh.manualRefresh()}
            >
              刷新
            </Button>
            <Button
              icon={<DownloadOutlined />}
              loading={exporting}
              onClick={() => void exportCsv()}
            >
              导出 CSV
            </Button>
            <Button onClick={() => setCleanupOpen(true)}>清理历史</Button>
          </>
        }
      />
      {newRequests > 0 && (
        <Alert
          className="new-records-alert"
          type="info"
          showIcon
          message={`有 ${newRequests} 条新请求`}
          action={<Button size="small" type="primary" onClick={showLatest}>查看最新</Button>}
          closable
          onClose={() => setNewRequests(0)}
        />
      )}
      <Card bordered={false} className="filter-card">
        <Space wrap>
          <Input.Search
            allowClear
            placeholder="模型名称"
            value={model}
            onChange={(event) => setModel(event.target.value)}
            onSearch={() => load(1)}
            style={{ width: 220 }}
          />
          <Input.Search
            allowClear
            placeholder="请求 ID"
            value={requestId}
            onChange={(event) => setRequestId(event.target.value)}
            onSearch={() => load(1)}
            style={{ width: 240 }}
          />
          <Select
            allowClear
            placeholder="提供商"
            value={providerId}
            onChange={(value) => { setProviderId(value); setTimeout(() => load(1), 0) }}
            options={providers.map((provider) => ({ value: provider.id, label: provider.name }))}
            style={{ width: 180 }}
          />
          <Select
            allowClear
            placeholder="调用结果"
            value={success}
            onChange={(value) => { setSuccess(value); setTimeout(() => load(1), 0) }}
            options={[
              { value: true, label: '成功' },
              { value: false, label: '失败' },
            ]}
            style={{ width: 130 }}
          />
          <DatePicker.RangePicker
            value={dates}
            onChange={(value) => setDates(value as [Dayjs, Dayjs] | undefined)}
            showTime
          />
          <Button type="primary" onClick={() => load(1)}>查询</Button>
        </Space>
      </Card>
      <Card bordered={false} className="table-card">
        <Table
          rowKey="id"
          loading={loading}
          dataSource={items}
          scroll={{ x: 1680 }}
          onRow={(record) => ({
            onClick: () => void openDetail(record),
            style: { cursor: 'pointer' },
          })}
          pagination={{
            current: page,
            pageSize,
            total,
            showSizeChanger: true,
            showTotal: (value) => `共 ${value} 条`,
            onChange: (nextPage, nextPageSize) => load(nextPage, nextPageSize),
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
                  {record.upstream_model && <div><Typography.Text type="secondary">{record.upstream_model}</Typography.Text></div>}
                </div>
              ),
            },
            {
              title: '提供商 / 路由',
              width: 170,
              render: (_, record) => (
                <div>
                  <div>{record.provider_name || (record.provider_id ? `#${record.provider_id}` : '-')}</div>
                  <Typography.Text type="secondary">
                    {record.route_name || (record.route_id ? `#${record.route_id}` : '自动路由')}
                  </Typography.Text>
                </div>
              ),
            },
            { title: '接口', dataIndex: 'endpoint', width: 180, render: (value: string) => <Typography.Text code>{value}</Typography.Text> },
            {
              title: '状态',
              dataIndex: 'success',
              width: 100,
              render: (value: boolean, record) => <Tag color={value ? 'success' : 'error'}>{value ? '成功' : record.status_code}</Tag>,
            },
            {
              title: '令牌',
              width: 150,
              render: (_, record) => {
                const cached = (record.cache_read_tokens || 0) + (record.cache_write_tokens || 0)
                return (
                  <div>
                    <Tooltip
                      title={`输入 ${formatExact(record.prompt_tokens)} / 输出 ${formatExact(record.completion_tokens)}`}
                    >
                      <span>{formatCompact(record.total_tokens)}</span>
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
              title: '延迟',
              dataIndex: 'latency_ms',
              width: 100,
              render: (value: number) => `${value} ms`,
            },
            {
              title: '费用',
              dataIndex: 'estimated_cost_micros',
              width: 90,
              render: (value: number | null) => formatCostMicros(value),
            },
            {
              title: '首 token',
              dataIndex: 'first_token_ms',
              width: 110,
              render: (value: number | undefined, record) =>
                value != null ? (
                  `${value} ms`
                ) : (
                  <Typography.Text type="secondary">{record.streamed ? '-' : '—'}</Typography.Text>
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
                  <span>{value != null ? `${value.toFixed(1)} tok/s` : '-'}</span>
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
              render: (value: string) => <Typography.Text copyable={{ text: value }}>{value.slice(0, 12)}…</Typography.Text>,
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
                        void openDetail(record)
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
      <Drawer
        title="请求详情"
        width={520}
        open={Boolean(detail)}
        onClose={() => setDetail(undefined)}
        loading={detailLoading}
      >
        {detail && (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <div className={`detail-status ${detail.success ? 'detail-status-success' : 'detail-status-error'}`}>
              {detail.success ? <CheckCircleOutlined /> : <CloseCircleOutlined />}
              <span>{detail.success ? '请求成功' : `请求失败 · HTTP ${detail.status_code}`}</span>
            </div>
            <Descriptions column={1} size="small" bordered>
              <Descriptions.Item label="请求 ID">
                <Typography.Text copyable={{ text: detail.request_id }}>{detail.request_id}</Typography.Text>
              </Descriptions.Item>
              <Descriptions.Item label="时间">
                {dayjs(detail.created_at).format('YYYY-MM-DD HH:mm:ss.SSS')}
              </Descriptions.Item>
              <Descriptions.Item label="接口">
                <Typography.Text code>{detail.endpoint}</Typography.Text>
              </Descriptions.Item>
              <Descriptions.Item label="请求模型">
                <Typography.Text code>{detail.requested_model}</Typography.Text>
              </Descriptions.Item>
              <Descriptions.Item label="上游模型">
                {detail.upstream_model || '-'}
              </Descriptions.Item>
              <Descriptions.Item label="提供商">
                {detail.provider_name || (detail.provider_id ? `#${detail.provider_id}` : '-')}
              </Descriptions.Item>
              <Descriptions.Item label="路由">
                {detail.route_name || (detail.route_id ? `#${detail.route_id}` : '自动路由')}
              </Descriptions.Item>
              <Descriptions.Item label="访问密钥">
                {detail.api_key_name || (detail.api_key_id ? `#${detail.api_key_id}` : '匿名调用')}
              </Descriptions.Item>
              <Descriptions.Item label="流式">
                <Tag>{detail.streamed ? '是' : '否'}</Tag>
              </Descriptions.Item>
              <Descriptions.Item label="延迟">
                {detail.latency_ms} ms
              </Descriptions.Item>
              <Descriptions.Item label="首 token 延迟">
                {detail.first_token_ms != null ? `${detail.first_token_ms} ms` : '-'}
              </Descriptions.Item>
              <Descriptions.Item label="生成速度 (TPS)">
                {detail.output_tps != null ? `${detail.output_tps.toFixed(1)} tok/s` : '-'}
              </Descriptions.Item>
              <Descriptions.Item label="输入 tokens">
                <Tooltip title={formatExact(detail.prompt_tokens)}>
                  {formatCompact(detail.prompt_tokens)}
                </Tooltip>
              </Descriptions.Item>
              <Descriptions.Item label="缓存读取 tokens">
                {detail.cache_read_tokens > 0 ? (
                  <Tooltip title={formatExact(detail.cache_read_tokens)}>
                    {formatCompact(detail.cache_read_tokens)}
                  </Tooltip>
                ) : (
                  '-'
                )}
              </Descriptions.Item>
              <Descriptions.Item label="缓存写入 tokens">
                {detail.cache_write_tokens > 0 ? (
                  <Tooltip title={formatExact(detail.cache_write_tokens)}>
                    {formatCompact(detail.cache_write_tokens)}
                  </Tooltip>
                ) : (
                  '-'
                )}
              </Descriptions.Item>
              <Descriptions.Item label="输出 tokens">
                <Tooltip title={formatExact(detail.completion_tokens)}>
                  {formatCompact(detail.completion_tokens)}
                </Tooltip>
              </Descriptions.Item>
              <Descriptions.Item label="总 tokens">
                <Tooltip title={formatExact(detail.total_tokens)}>
                  {formatCompact(detail.total_tokens)}
                </Tooltip>
              </Descriptions.Item>
              <Descriptions.Item label="预估费用">
                {formatCostMicros(detail.estimated_cost_micros)}
              </Descriptions.Item>
            </Descriptions>
            {detail.error_message && (
              <div>
                <Typography.Text strong>错误信息</Typography.Text>
                <pre className="error-block">{detail.error_message}</pre>
              </div>
            )}
            {detail.response_preview && (
              <div>
                <Space style={{ width: '100%', justifyContent: 'space-between' }}>
                  <Typography.Text strong>响应内容预览</Typography.Text>
                  <Button
                    type="link"
                    size="small"
                    icon={<CopyOutlined />}
                    onClick={() => {
                      void navigator.clipboard.writeText(detail.response_preview || '')
                      message.success('已复制')
                    }}
                  >
                    复制
                  </Button>
                </Space>
                <pre className="response-block">{detail.response_preview}</pre>
              </div>
            )}
          </Space>
        )}
      </Drawer>
      <Modal
        title="清理历史日志"
        open={cleanupOpen}
        onCancel={() => setCleanupOpen(false)}
        onOk={() => void runCleanup()}
        confirmLoading={cleaning}
        okText="开始清理"
        okButtonProps={{ danger: true }}
        destroyOnHidden
      >
        <Alert
          type="warning"
          showIcon
          message="此操作不可撤销"
          description="将永久删除早于指定天数的用量记录，仪表盘的历史统计也会随之减少。"
          style={{ marginBottom: 16 }}
        />
        <Space>
          <Typography.Text>保留最近</Typography.Text>
          <InputNumber
            min={1}
            max={3650}
            value={cleanupDays}
            onChange={setCleanupDays}
            addonAfter="天"
          />
        </Space>
      </Modal>
    </>
  )
}
