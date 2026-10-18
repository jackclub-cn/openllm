import { Input, Select, Space, Switch, Typography } from 'antd'
import type { ApiKey, GatewayRoute, Provider } from '../api'
import type { UsageStatusFilter } from '../pages/usage/types'

export type UsageDimensionFilterValues = {
  model: string
  endpoint: string
  providerId?: number
  providerApiKeyId?: number
  apiKeyId?: number
  routeId?: number
  statusFilter: UsageStatusFilter
  onlyAdjusted: boolean
}

type UsageDimensionFiltersProps = {
  values: UsageDimensionFilterValues
  providers: Provider[]
  apiKeys: ApiKey[]
  routes: GatewayRoute[]
  onChange: (patch: Partial<UsageDimensionFilterValues>) => void
  onProviderChange: (value?: number) => void
  onProviderApiKeyChange: (value?: number) => void
  onSearch?: () => void
  /** Analytics only aggregates completed requests, so it hides this option. */
  includePending?: boolean
}

export default function UsageDimensionFilters({
  values,
  providers,
  apiKeys,
  routes,
  onChange,
  onProviderChange,
  onProviderApiKeyChange,
  onSearch,
  includePending = true,
}: UsageDimensionFiltersProps) {
  const statusOptions: Array<{ value: UsageStatusFilter; label: string }> = [
    { value: 'all', label: '全部状态' },
    { value: 'success', label: '成功' },
    { value: 'failed', label: '失败' },
  ]
  if (includePending) {
    statusOptions.push({ value: 'pending', label: '请求中' })
  }

  return (
    <>
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
        onChange={(value: UsageStatusFilter) => onChange({ statusFilter: value })}
        options={statusOptions}
        style={{ width: 140 }}
      />
      <Space size={6}>
        <Switch
          checked={values.onlyAdjusted}
          onChange={(value) => onChange({ onlyAdjusted: value })}
        />
        <Typography.Text>网关调整</Typography.Text>
      </Space>
    </>
  )
}
