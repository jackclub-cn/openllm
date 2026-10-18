import { ClearOutlined } from '@ant-design/icons'
import { Button, Card, DatePicker, Input, Space } from 'antd'
import type { Dayjs } from 'dayjs'
import type { ApiKey, GatewayRoute, Provider } from '../../api'
import UsageDimensionFilters, {
  type UsageDimensionFilterValues,
} from '../../components/UsageDimensionFilters'

type UsageFilterValues = UsageDimensionFilterValues & {
  requestId: string
  sessionId: string
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
        <UsageDimensionFilters
          values={values}
          providers={providers}
          apiKeys={apiKeys}
          routes={routes}
          onChange={onChange}
          onProviderChange={onProviderChange}
          onProviderApiKeyChange={onProviderApiKeyChange}
          onSearch={onSearch}
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
