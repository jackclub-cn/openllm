import { useEffect, useState } from 'react'
import {
  CheckCircleOutlined,
  ClearOutlined,
  SearchOutlined,
  StopOutlined,
} from '@ant-design/icons'
import {
  App,
  Button,
  Input,
  InputNumber,
  Modal,
  Segmented,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  type Provider,
  type ProviderModelLimit,
  type ProviderModelLimitInput,
} from '../../api'
import { formatCompact, formatExact } from '../../format'
import { endpointOptions } from './types'

type ProviderLimitsModalProps = {
  provider?: Provider
  onClose: () => void
}

export default function ProviderLimitsModal({
  provider,
  onClose,
}: ProviderLimitsModalProps) {
  const { message } = App.useApp()
  const [loading, setLoading] = useState(false)
  const [saving, setSaving] = useState(false)
  const [rows, setRows] = useState<ProviderModelLimit[]>([])
  const [search, setSearch] = useState('')
  const [status, setStatus] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [page, setPage] = useState(1)

  useEffect(() => {
    if (!provider) return
    let cancelled = false
    setSearch('')
    setStatus('all')
    setPage(1)
    setLoading(true)
    api
      .get<ProviderModelLimit[]>(`/api/providers/${provider.id}/model-limits`)
      .then((next) => {
        if (!cancelled) setRows(next)
      })
      .catch((error) => {
        if (cancelled) return
        message.error(formatError(error))
        onClose()
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [provider])

  const updateLimitRow = (
    modelName: string,
    field: 'context_override' | 'input_override' | 'output_override',
    value: number | null,
  ) => {
    setRows((current) =>
      current.map((row) =>
        row.model_name === modelName ? { ...row, [field]: value ?? undefined } : row,
      ),
    )
  }

  const updateEndpointOverride = (modelName: string, value?: string[]) => {
    setRows((current) =>
      current.map((row) =>
        row.model_name === modelName
          ? { ...row, supported_endpoints_override: value?.length ? value : undefined }
          : row,
      ),
    )
  }

  const updateCostOverride = (
    modelName: string,
    field:
      | 'cost_input_override'
      | 'cost_output_override'
      | 'cost_cache_read_override'
      | 'cost_cache_write_override',
    value: number | null,
  ) => {
    setRows((current) =>
      current.map((row) =>
        row.model_name === modelName ? { ...row, [field]: value ?? undefined } : row,
      ),
    )
  }

  const updateConcurrency = (
    modelName: string,
    field: 'max_concurrency' | 'queue_timeout_seconds',
    value: number | null,
  ) => {
    setRows((current) =>
      current.map((row) =>
        row.model_name === modelName ? { ...row, [field]: value ?? undefined } : row,
      ),
    )
  }

  const toggleLimitRow = (modelName: string, enabled: boolean) => {
    setRows((current) =>
      current.map((row) => (row.model_name === modelName ? { ...row, enabled } : row)),
    )
  }

  const filteredRows = rows.filter((row) => {
    if (!row.model_name.toLowerCase().includes(search.trim().toLowerCase())) return false
    if (status === 'enabled') return row.enabled
    if (status === 'disabled') return !row.enabled
    return true
  })

  const setFilteredRowsEnabled = (enabled: boolean) => {
    const names = new Set(filteredRows.map((row) => row.model_name))
    setRows((current) =>
      current.map((row) => (names.has(row.model_name) ? { ...row, enabled } : row)),
    )
  }

  const clearFilteredOverrides = () => {
    const names = new Set(filteredRows.map((row) => row.model_name))
    setRows((current) =>
      current.map((row) =>
        names.has(row.model_name)
          ? {
              ...row,
              context_override: undefined,
              input_override: undefined,
              output_override: undefined,
              max_concurrency: undefined,
              queue_timeout_seconds: undefined,
              supported_endpoints_override: undefined,
              cost_input_override: undefined,
              cost_output_override: undefined,
              cost_cache_read_override: undefined,
              cost_cache_write_override: undefined,
            }
          : row,
      ),
    )
  }

  const save = async () => {
    if (!provider) return
    const models: ProviderModelLimitInput[] = rows.map((row) => ({
      model_name: row.model_name,
      enabled: row.enabled,
      supported_endpoints_override: row.supported_endpoints_override ?? null,
      context_limit: row.context_override ?? null,
      input_limit: row.input_override ?? null,
      output_limit: row.output_override ?? null,
      max_concurrency: row.max_concurrency ?? null,
      queue_timeout_seconds: row.queue_timeout_seconds ?? null,
      cost_input_override: row.cost_input_override ?? null,
      cost_output_override: row.cost_output_override ?? null,
      cost_cache_read_override: row.cost_cache_read_override ?? null,
      cost_cache_write_override: row.cost_cache_write_override ?? null,
    }))
    setSaving(true)
    try {
      setRows(
        await api.put<ProviderModelLimit[]>(`/api/providers/${provider.id}/model-limits`, {
          models,
        }),
      )
      message.success('模型设置已保存')
      onClose()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Modal
      title={provider ? `模型管理 · ${provider.name}` : '模型管理'}
      open={Boolean(provider)}
      onCancel={onClose}
      onOk={() => void save()}
      confirmLoading={saving}
      okButtonProps={{ disabled: loading }}
      width={960}
      destroyOnHidden
    >
      <Typography.Paragraph type="secondary">
        停用后模型不会出现在 /v1/models 或参与路由。上限、接口和价格留空使用同步值，填写后覆盖同步值。
      </Typography.Paragraph>
      <Space wrap style={{ marginBottom: 12 }}>
        <Input
          allowClear
          prefix={<SearchOutlined />}
          value={search}
          onChange={(event) => {
            setSearch(event.target.value)
            setPage(1)
          }}
          placeholder="搜索模型"
          style={{ width: 280 }}
        />
        <Segmented
          value={status}
          onChange={(value) => {
            setStatus(value as 'all' | 'enabled' | 'disabled')
            setPage(1)
          }}
          options={[
            { label: '全部', value: 'all' },
            { label: '已启用', value: 'enabled' },
            { label: '已停用', value: 'disabled' },
          ]}
        />
        <Tag>{filteredRows.length} 个结果</Tag>
        <Button
          icon={<CheckCircleOutlined />}
          disabled={!filteredRows.length}
          onClick={() => setFilteredRowsEnabled(true)}
        >
          批量启用
        </Button>
        <Button
          icon={<StopOutlined />}
          disabled={!filteredRows.length}
          onClick={() => setFilteredRowsEnabled(false)}
        >
          批量停用
        </Button>
        <Button
          icon={<ClearOutlined />}
          disabled={!filteredRows.length}
          onClick={clearFilteredOverrides}
        >
          清除覆盖
        </Button>
      </Space>
      <Table
        rowKey="model_name"
        size="small"
        loading={loading}
        dataSource={filteredRows}
        pagination={{
          current: page,
          defaultPageSize: 50,
          pageSizeOptions: [20, 50, 100],
          showSizeChanger: true,
          showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
          onChange: setPage,
        }}
        scroll={{ x: 1930, y: 520 }}
        locale={{ emptyText: '暂无模型' }}
        columns={[
          {
            title: '模型',
            dataIndex: 'model_name',
            width: 250,
            render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
          },
          {
            title: '支持接口',
            dataIndex: 'supported_endpoints',
            width: 230,
            render: (value: string[]) =>
              value.length ? (
                <Space size={4} wrap>
                  {value.map((endpoint) => (
                    <Tag key={endpoint}>{endpoint.replace(/^\/v1/, '') || endpoint}</Tag>
                  ))}
                </Space>
              ) : (
                <Typography.Text type="secondary">未声明</Typography.Text>
              ),
          },
          {
            title: '接口覆盖',
            width: 240,
            render: (_, record) => (
              <Select
                mode="multiple"
                allowClear
                maxTagCount="responsive"
                disabled={!record.enabled}
                value={record.supported_endpoints_override ?? undefined}
                placeholder="使用同步值"
                options={endpointOptions}
                onChange={(value) => updateEndpointOverride(record.model_name, value)}
                style={{ width: '100%' }}
              />
            ),
          },
          {
            title: '启用',
            dataIndex: 'enabled',
            width: 70,
            render: (value: boolean, record) => (
              <Switch
                size="small"
                checked={value}
                onChange={(checked) => toggleLimitRow(record.model_name, checked)}
              />
            ),
          },
          {
            title: '当前生效',
            key: 'effective',
            width: 210,
            render: (_, record) => (
              <Space size={4} wrap>
                {record.context_limit != null && (
                  <Tooltip title={`上下文 ${formatExact(record.context_limit)}`}>
                    <Tag>上下文 {formatCompact(record.context_limit)}</Tag>
                  </Tooltip>
                )}
                {record.input_limit != null && (
                  <Tooltip title={`输入 ${formatExact(record.input_limit)}`}>
                    <Tag>输入 {formatCompact(record.input_limit)}</Tag>
                  </Tooltip>
                )}
                {record.output_limit != null && (
                  <Tooltip title={`输出 ${formatExact(record.output_limit)}`}>
                    <Tag>输出 {formatCompact(record.output_limit)}</Tag>
                  </Tooltip>
                )}
                {record.context_limit == null &&
                  record.input_limit == null &&
                  record.output_limit == null && (
                    <Typography.Text type="secondary">无</Typography.Text>
                  )}
              </Space>
            ),
          },
          {
            title: '上下文覆盖',
            width: 150,
            render: (_, record) => (
              <InputNumber
                min={1}
                disabled={!record.enabled}
                value={record.context_override}
                placeholder={record.context_limit?.toString()}
                onChange={(value) =>
                  updateLimitRow(record.model_name, 'context_override', value)
                }
                style={{ width: '100%' }}
              />
            ),
          },
          {
            title: '输入覆盖',
            width: 150,
            render: (_, record) => (
              <InputNumber
                min={1}
                disabled={!record.enabled}
                value={record.input_override}
                placeholder={record.input_limit?.toString()}
                onChange={(value) => updateLimitRow(record.model_name, 'input_override', value)}
                style={{ width: '100%' }}
              />
            ),
          },
          {
            title: '输出覆盖',
            width: 150,
            render: (_, record) => (
              <InputNumber
                min={1}
                disabled={!record.enabled}
                value={record.output_override}
                placeholder={record.output_limit?.toString()}
                onChange={(value) =>
                  updateLimitRow(record.model_name, 'output_override', value)
                }
                style={{ width: '100%' }}
              />
            ),
          },
          {
            title: '模型并发',
            width: 240,
            render: (_, record) => (
              <Space direction="vertical" size={4} style={{ width: '100%' }}>
                <InputNumber
                  min={0}
                  max={1000}
                  precision={0}
                  disabled={!record.enabled}
                  value={record.max_concurrency}
                  placeholder="不限"
                  addonBefore="并发"
                  onChange={(value) =>
                    updateConcurrency(record.model_name, 'max_concurrency', value)
                  }
                  style={{ width: '100%' }}
                />
                <InputNumber
                  min={0}
                  max={300}
                  precision={0}
                  disabled={!record.enabled || !record.max_concurrency}
                  value={record.queue_timeout_seconds}
                  placeholder="30"
                  addonBefore="等待"
                  addonAfter="秒"
                  onChange={(value) =>
                    updateConcurrency(record.model_name, 'queue_timeout_seconds', value)
                  }
                  style={{ width: '100%' }}
                />
              </Space>
            ),
          },
          {
            title: '价格覆盖（USD / 1M）',
            width: 320,
            render: (_, record) => (
              <Space direction="vertical" size={4} style={{ width: '100%' }}>
                <InputNumber
                  min={0}
                  precision={6}
                  disabled={!record.enabled}
                  value={record.cost_input_override}
                  placeholder={record.cost_input?.toString() ?? '输入'}
                  addonBefore="输入"
                  onChange={(value) =>
                    updateCostOverride(record.model_name, 'cost_input_override', value)
                  }
                  style={{ width: '100%' }}
                />
                <InputNumber
                  min={0}
                  precision={6}
                  disabled={!record.enabled}
                  value={record.cost_output_override}
                  placeholder={record.cost_output?.toString() ?? '输出'}
                  addonBefore="输出"
                  onChange={(value) =>
                    updateCostOverride(record.model_name, 'cost_output_override', value)
                  }
                  style={{ width: '100%' }}
                />
                <InputNumber
                  min={0}
                  precision={6}
                  disabled={!record.enabled}
                  value={record.cost_cache_read_override}
                  placeholder={record.cost_cache_read?.toString() ?? '缓存读'}
                  addonBefore="缓存读"
                  onChange={(value) =>
                    updateCostOverride(record.model_name, 'cost_cache_read_override', value)
                  }
                  style={{ width: '100%' }}
                />
                <InputNumber
                  min={0}
                  precision={6}
                  disabled={!record.enabled}
                  value={record.cost_cache_write_override}
                  placeholder={record.cost_cache_write?.toString() ?? '缓存写'}
                  addonBefore="缓存写"
                  onChange={(value) =>
                    updateCostOverride(record.model_name, 'cost_cache_write_override', value)
                  }
                  style={{ width: '100%' }}
                />
              </Space>
            ),
          },
          {
            title: '状态',
            width: 80,
            render: (_, record) =>
              !record.enabled ? (
                <Tag color="default">已停用</Tag>
              ) : record.context_override != null ||
                record.input_override != null ||
                record.output_override != null ||
                record.max_concurrency != null ||
                record.queue_timeout_seconds != null ||
                record.supported_endpoints_override != null ||
                record.cost_input_override != null ||
                record.cost_output_override != null ||
                record.cost_cache_read_override != null ||
                record.cost_cache_write_override != null ? (
                <Tag color="blue">已覆盖</Tag>
              ) : (
                <Tag>同步值</Tag>
              ),
          },
        ]}
      />
    </Modal>
  )
}
