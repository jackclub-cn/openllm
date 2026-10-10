import { ClearOutlined } from '@ant-design/icons'
import { Button, Card, DatePicker, Input, Select, Space, Switch, Typography } from 'antd'
import type { Dayjs } from 'dayjs'
import type { ApiKey, GatewayRoute, Provider } from '../../api'
import type { UsageStatusFilter } from './types'

type UsageFilterValues = {
  model: string
  requestId: string
  sessionId: string
  endpoint: string
  providerId?: number
  providerApiKeyId?: number
  apiKeyId?: number
  routeId?: number
  statusFilter: UsageStatusFilter
  onlyAdjusted: boolean
  dates?: [Dayjs, Dayjs]
}

type UsageFiltersProps = {
  values: UsageFilterValues
  providers: Provider[]
  apiKeys: ApiKey[]
  routes: GatewayRoute[]
  onChange: (patch: Partial<UsageFilterValues>) => void
  onProviderChange: (value?: number) => void
  onProviderApiKeyChange: (value?: number) => void
  onReset: () => void
  onSearch: () => void
}

export default function UsageFilters({
  values,
  providers,
  apiKeys,
  routes,
  onChange,
  onProviderChange,
  onProviderApiKeyChange,
  onReset,
  onSearch,
}: UsageFiltersProps) {
  return (
    <Card bordered={false} className="filter-card">
      <Space wrap>
        <Input.Search
          allowClear
          placeholder="模型名称"
          value={values.model}
          onChange={(event) => onChange({ model: event.target.value })}
          onSearch={onSearch}
          style={{ width: 220 }}
        />
        <Input.Search
          allowClear
          placeholder="请求 ID"
          value={values.requestId}
          onChange={(event) => onChange({ requestId: event.target.value })}
          onSearch={onSearch}
          style={{ width: 240 }}
        />
        <Input.Search
          allowClear
          placeholder="会话 ID"
          value={values.sessionId}
          onChange={(event) => onChange({ sessionId: event.target.value })}
          onSearch={onSearch}
          style={{ width: 220 }}
        />
        <Input.Search
          allowClear
          placeholder="接口路径"
          value={values.endpoint}
          onChange={(event) => onChange({ endpoint: event.target.value })}
          onSearch={onSearch}
          style={{ width: 220 }}
        />
        <Select
          allowClear
          placeholder="提供商"
          value={values.providerId}
          onChange={onProviderChange}
          options={providers.map((provider) => ({
            value: provider.id,
            label: provider.name,
          }))}
          style={{ width: 180 }}
        />
        <Select
          allowClear
          showSearch
          optionFilterProp="label"
          placeholder="上游密钥"
          value={values.providerApiKeyId}
          onChange={onProviderApiKeyChange}
          options={providers
            .filter((provider) => !values.providerId || provider.id === values.providerId)
            .flatMap((provider) =>
              provider.api_keys.map((key) => ({
                value: key.id,
                label: `${provider.name} · ${key.name || `Key ${key.id}`} (${key.api_key_suffix})`,
              })),
            )}
          style={{ width: 220 }}
        />
        <Select
          allowClear
          showSearch
          optionFilterProp="label"
          placeholder="访问密钥"
          value={values.apiKeyId}
          onChange={(value) => onChange({ apiKeyId: value })}
          options={apiKeys.map((item) => ({ value: item.id, label: item.name }))}
          style={{ width: 180 }}
        />
        <Select
          allowClear
          showSearch
          optionFilterProp="label"
          placeholder="路由"
          value={values.routeId}
          onChange={(value) => onChange({ routeId: value })}
          options={routes.map((item) => ({ value: item.id, label: item.name }))}
          style={{ width: 180 }}
        />
        <Select
          placeholder="调用结果"
          value={values.statusFilter}
          onChange={(value) => onChange({ statusFilter: value })}
          options={[
            { value: 'all', label: '全部状态' },
            { value: 'success', label: '成功' },
            { value: 'failed', label: '失败' },
            { value: 'pending', label: '请求中' },
          ]}
          style={{ width: 140 }}
        />
        <Space size={6}>
          <Switch
            checked={values.onlyAdjusted}
            onChange={(value) => onChange({ onlyAdjusted: value })}
          />
          <Typography.Text>网关调整</Typography.Text>
        </Space>
        <DatePicker.RangePicker
          value={values.dates}
          onChange={(value) =>
            onChange({ dates: value as [Dayjs, Dayjs] | undefined })
          }
          showTime
        />
        <Button icon={<ClearOutlined />} onClick={onReset}>
          重置
        </Button>
        <Button type="primary" onClick={onSearch}>
          查询
        </Button>
      </Space>
    </Card>
  )
}
