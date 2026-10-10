import {
  ApiOutlined,
  DeleteOutlined,
  EditOutlined,
  HistoryOutlined,
  KeyOutlined,
  SettingOutlined,
  SyncOutlined,
  ThunderboltOutlined,
  WalletOutlined,
} from '@ant-design/icons'
import { Button, Card, Popconfirm, Space, Switch, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { useNavigate } from 'react-router-dom'
import type { Provider } from '../../api'
import { formatCompact } from '../../format'
import { providerLabels } from './types'

type ProviderTableProps = {
  providers: Provider[]
  providerCount: number
  loading: boolean
  page: number
  togglingProviderId?: number
  testingProviderId?: number
  onPageChange: (page: number) => void
  onToggleEnabled: (provider: Provider, enabled: boolean) => void
  onTest: (id: number) => void
  onTestKeys: (provider: Provider) => void
  onOpenQuota: (provider: Provider) => void
  onManageModels: (provider: Provider) => void
  onEdit: (provider: Provider) => void
  onRemove: (id: number) => void
  onPreviewSync: (provider: Provider) => void
}

export default function ProviderTable({
  providers,
  providerCount,
  loading,
  page,
  togglingProviderId,
  testingProviderId,
  onPageChange,
  onToggleEnabled,
  onTest,
  onTestKeys,
  onOpenQuota,
  onManageModels,
  onEdit,
  onRemove,
  onPreviewSync,
}: ProviderTableProps) {
  const navigate = useNavigate()

  return (
    <Card bordered={false}>
      <Table
        rowKey="id"
        loading={loading}
        dataSource={providers}
        pagination={{
          current: page,
          defaultPageSize: 20,
          pageSizeOptions: [10, 20, 50, 100],
          showSizeChanger: true,
          showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
          onChange: onPageChange,
        }}
        locale={{
          emptyText: providerCount ? '没有符合筛选条件的提供商' : '暂无提供商',
        }}
        scroll={{ x: 1190 }}
        columns={[
          {
            title: '提供商',
            dataIndex: 'name',
            render: (value: string, record) => (
              <Space>
                <ApiOutlined />
                <div>
                  <Typography.Text strong>{value}</Typography.Text>
                  <div>
                    <Typography.Text type="secondary">
                      {providerLabels[record.provider_type]}
                    </Typography.Text>
                  </div>
                </div>
              </Space>
            ),
          },
          {
            title: 'API 地址',
            dataIndex: 'base_url',
            render: (value: string) => (
              <Typography.Text copyable={{ text: value }}>{value}</Typography.Text>
            ),
          },
          {
            title: '凭证',
            dataIndex: 'api_keys',
            width: 130,
            render: (keys: Provider['api_keys']) => {
              if (!keys?.length) {
                return <Tag color="default">无需密钥</Tag>
              }
              const enabled = keys.filter((key) => key.enabled).length
              const cooling = keys.filter((key) => key.cooldown_seconds).length
              const failedTests = keys.filter(
                (key) => key.enabled && key.last_test_ok === false,
              ).length
              const detail = keys.map((key) => (
                <div key={key.id}>
                  {key.name || `Key ${key.id}`} · {key.api_key_suffix || '****'} ·{' '}
                  {key.enabled ? '启用' : '停用'}
                  {key.cooldown_seconds ? ` · 冷却 ${key.cooldown_seconds}s` : ''}
                  {key.last_test_ok != null
                    ? ` · 检测${key.last_test_ok ? '正常' : '异常'} ${key.last_test_latency_ms ?? 0} ms`
                    : ''}
                  {key.requests > 0
                    ? ` · ${key.requests} 次 · ${key.success_rate.toFixed(1)}% · ${Math.round(key.avg_latency_ms)} ms · 输入 ${formatCompact(key.prompt_tokens)} / 输出 ${formatCompact(key.completion_tokens)}`
                    : ' · 暂无请求'}
                  {key.last_test_ok === false && key.last_test_message
                    ? ` · ${key.last_test_message}`
                    : ''}
                  {key.last_error ? ` · ${key.last_error}` : ''}
                </div>
              ))
              return (
                <Tooltip title={<div>{detail}</div>}>
                  <Tag
                    color={
                      failedTests ? 'red' : cooling ? 'orange' : enabled ? 'green' : 'default'
                    }
                  >
                    {enabled}/{keys.length} 个可用
                    {failedTests ? ` · ${failedTests} 检测异常` : ''}
                    {cooling ? ` · ${cooling} 冷却` : ''}
                  </Tag>
                </Tooltip>
              )
            },
          },
          {
            title: '健康',
            width: 160,
            render: (_, record) => {
              const state =
                record.last_test_ok == null
                  ? { color: 'default', label: '未检测' }
                  : record.last_test_ok
                    ? { color: 'success', label: '正常' }
                    : { color: 'error', label: '异常' }
              const detail = [
                record.last_test_message,
                record.last_test_latency_ms != null ? `${record.last_test_latency_ms} ms` : '',
                record.health_check_model ? `检测模型 ${record.health_check_model}` : '',
                record.last_test_checked === 'models' ? '仅验证主机可达' : '',
                record.health_check_interval_minutes
                  ? `自动每 ${record.health_check_interval_minutes} 分钟`
                  : '',
                record.timeout_seconds ? `请求超时 ${record.timeout_seconds}s` : '',
                record.configured_cooldown_seconds
                  ? `故障冷却基准 ${record.configured_cooldown_seconds}s`
                  : '',
              ]
                .filter(Boolean)
                .join(' · ')
              return (
                <Tooltip title={detail || undefined}>
                  <Space direction="vertical" size={0}>
                    <Tag color={state.color}>{state.label}</Tag>
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      {record.last_test_at
                        ? dayjs(record.last_test_at).format('YYYY-MM-DD HH:mm')
                        : '尚未检测'}
                    </Typography.Text>
                  </Space>
                </Tooltip>
              )
            },
          },
          {
            title: '模型',
            dataIndex: 'models',
            width: 190,
            render: (models: string[], record) => {
              const syncing =
                !record.models_sync_error &&
                Boolean(record.models_sync_attempted_at) &&
                (!record.models_synced_at ||
                  dayjs(record.models_sync_attempted_at).isAfter(dayjs(record.models_synced_at)))
              const syncDetail = record.models_sync_error
                ? record.models_sync_error
                : syncing
                  ? `开始于 ${dayjs(record.models_sync_attempted_at).format('YYYY-MM-DD HH:mm:ss')}`
                  : record.models_synced_at
                    ? `完成于 ${dayjs(record.models_synced_at).format('YYYY-MM-DD HH:mm:ss')}`
                    : undefined
              return (
                <div>
                  <div>{models.length ? `${models.length} 个` : '-'}</div>
                  <Tooltip title={syncDetail}>
                    <Typography.Text
                      type={record.models_sync_error ? 'danger' : syncing ? 'warning' : 'secondary'}
                      ellipsis
                      style={{ maxWidth: 170 }}
                    >
                      {record.models_sync_error
                        ? `同步失败：${record.models_sync_error}`
                        : syncing
                          ? '同步中…'
                          : record.models_synced_at
                            ? `同步于 ${dayjs(record.models_synced_at).format('YYYY-MM-DD HH:mm')}`
                            : '尚未同步'}
                    </Typography.Text>
                  </Tooltip>
                </div>
              )
            },
          },
          {
            title: '前缀',
            dataIndex: 'model_prefix',
            width: 120,
            render: (value: string) =>
              value ? (
                <Typography.Text code>{value}</Typography.Text>
              ) : (
                <Typography.Text type="secondary">无</Typography.Text>
              ),
          },
          {
            title: '状态',
            dataIndex: 'enabled',
            width: 120,
            render: (value: boolean, record) => (
              <Space direction="vertical" size={4}>
                <Switch
                  checked={value}
                  loading={togglingProviderId === record.id}
                  checkedChildren="启用"
                  unCheckedChildren="停用"
                  onChange={(checked) => onToggleEnabled(record, checked)}
                />
                {record.cooldown_seconds ? (
                  <Tag color="orange">冷却 {record.cooldown_seconds}s</Tag>
                ) : null}
              </Space>
            ),
          },
          {
            title: '操作',
            width: 360,
            fixed: 'right',
            render: (_, record) => (
              <Space>
                <Tooltip title="查看请求日志">
                  <Button
                    type="text"
                    icon={<HistoryOutlined />}
                    onClick={() => navigate(`/usage?provider_id=${record.id}`)}
                  />
                </Tooltip>
                <Tooltip title="预览模型同步">
                  <Button
                    type="text"
                    icon={<SyncOutlined />}
                    onClick={() => onPreviewSync(record)}
                  />
                </Tooltip>
                <Tooltip title="测试连接">
                  <Button
                    type="text"
                    icon={<ThunderboltOutlined />}
                    loading={testingProviderId === record.id}
                    onClick={() => onTest(record.id)}
                  />
                </Tooltip>
                {record.api_keys.length > 0 && (
                  <Tooltip title="逐个测试全部启用密钥">
                    <Button
                      type="text"
                      icon={<KeyOutlined />}
                      disabled={!record.api_keys.some((key) => key.enabled)}
                      onClick={() => onTestKeys(record)}
                    />
                  </Tooltip>
                )}
                {record.quota_kind && (
                  <Tooltip title="查看额度、余额和价格">
                    <Button
                      type="text"
                      icon={<WalletOutlined />}
                      onClick={() => onOpenQuota(record)}
                    />
                  </Tooltip>
                )}
                <Tooltip title="模型管理">
                  <Button
                    type="text"
                    icon={<SettingOutlined />}
                    onClick={() => onManageModels(record)}
                  />
                </Tooltip>
                <Tooltip title="编辑">
                  <Button type="text" icon={<EditOutlined />} onClick={() => onEdit(record)} />
                </Tooltip>
                <Popconfirm title="删除此提供商？" onConfirm={() => onRemove(record.id)}>
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
