import { Form, Input, Select } from 'antd'

const STRATEGY_OPTIONS = [
  { value: 'priority', label: 'priority · 按优先级' },
  { value: 'weighted', label: 'weighted · 按权重随机' },
  { value: 'round_robin', label: 'round_robin · 轮询' },
  { value: 'cost_optimized', label: 'cost_optimized · 成本优先' },
  { value: 'latency_optimized', label: 'latency_optimized · 延迟优先' },
  { value: 'least_used', label: 'least_used · 最少使用' },
]

/// Routing constraints shared by the create and edit dialogs. The same policy
/// can be overridden per request with the `x-openllm-*` headers.
export default function ApiKeyRoutingFields() {
  return (
    <>
      <Form.Item
        name="routing_strategy"
        label="路由策略"
        extra="留空表示沿用路由自身配置。"
      >
        <Select
          allowClear
          placeholder="继承路由配置"
          options={STRATEGY_OPTIONS}
        />
      </Form.Item>
      <Form.Item
        name="routing_provider"
        label="固定提供商"
        extra="填写提供商名称或数字 ID，留空表示不固定。"
      >
        <Input placeholder="例如 openai 或 3" />
      </Form.Item>
      <Form.Item
        name="routing_exclude_providers_text"
        label="排除提供商"
        extra="每行一个提供商名称或数字 ID，留空表示不排除。"
      >
        <Input.TextArea rows={2} placeholder={'例如\nbackup-provider'} />
      </Form.Item>
    </>
  )
}
