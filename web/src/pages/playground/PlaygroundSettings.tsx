import { Card, InputNumber, Select, Slider, Space, Tag, Tooltip, Typography } from 'antd'
import type { ApiKey, ModelInfo } from '../../api'
import { formatCompact, formatExact } from '../../format'
import type { ModelOption } from './types'

type PlaygroundSettingsProps = {
  models: ModelOption[]
  apiKeys: ApiKey[]
  model?: string
  gatewayKeyId?: number
  temperature: number
  maxTokens: number | null
  budgetUsd: number | null
  outputLimit?: number
  capabilities?: ModelInfo['capabilities']
  selected?: ModelInfo
  onModelChange: (value?: string) => void
  onGatewayKeyChange: (value?: number) => void
  onTemperatureChange: (value: number) => void
  onMaxTokensChange: (value: number | null) => void
  onBudgetUsdChange: (value: number | null) => void
}

export default function PlaygroundSettings({
  models,
  apiKeys,
  model,
  gatewayKeyId,
  temperature,
  maxTokens,
  budgetUsd,
  outputLimit,
  capabilities,
  selected,
  onModelChange,
  onGatewayKeyChange,
  onTemperatureChange,
  onMaxTokensChange,
  onBudgetUsdChange,
}: PlaygroundSettingsProps) {
  return (
    <Card title="请求参数" bordered={false} className="playground-settings">
      <Space direction="vertical" size={20} style={{ width: '100%' }}>
        <div>
          <Typography.Text strong>模型</Typography.Text>
          <Select
            className="settings-control"
            showSearch
            value={model}
            onChange={onModelChange}
            placeholder="选择模型"
            options={models.map((item) => ({ value: item.id, label: item.id }))}
          />
        </div>
        <div>
          <Typography.Text strong>网关 API Key</Typography.Text>
          <Select
            className="settings-control"
            showSearch
            optionFilterProp="label"
            value={gatewayKeyId}
            onChange={onGatewayKeyChange}
            placeholder={apiKeys.length ? '选择网关 API Key' : '未启用网关鉴权'}
            disabled={!apiKeys.length}
            options={apiKeys.map((key) => {
              const expired =
                Boolean(key.expires_at) &&
                new Date(key.expires_at as string).getTime() <= Date.now()
              return {
                value: key.id,
                label: `${key.name} (${key.key_suffix})${key.enabled && !expired ? '' : ' · 不可用'}`,
                disabled: !key.enabled || expired,
              }
            })}
          />
        </div>
        <div>
          <Typography.Text strong>Temperature</Typography.Text>
          <Slider
            min={0}
            max={2}
            step={0.1}
            value={temperature}
            onChange={onTemperatureChange}
          />
          <Typography.Text type="secondary">{temperature.toFixed(1)}</Typography.Text>
        </div>
        <div>
          <Typography.Text strong>最大输出 tokens</Typography.Text>
          <InputNumber
            className="settings-control"
            min={1}
            max={outputLimit ?? 131072}
            value={maxTokens && outputLimit ? Math.min(maxTokens, outputLimit) : maxTokens}
            onChange={onMaxTokensChange}
          />
        </div>
        <div>
          <Tooltip title="按目标价格估算本次请求，超过该美元上限的目标会被跳过；留空表示不限制。">
            <Typography.Text strong>成本预算 (USD)</Typography.Text>
          </Tooltip>
          <InputNumber
            className="settings-control"
            min={0}
            step={0.001}
            placeholder="不限制"
            value={budgetUsd}
            onChange={onBudgetUsdChange}
          />
        </div>
        {capabilities && (
          <div>
            <Typography.Text strong>模型能力</Typography.Text>
            <div className="capability-tags">
              {capabilities.context_limit != null && (
                <Tooltip title={formatExact(capabilities.context_limit)}>
                  <Tag>上下文 {formatCompact(capabilities.context_limit)}</Tag>
                </Tooltip>
              )}
              {capabilities.output_limit != null && (
                <Tooltip title={formatExact(capabilities.output_limit)}>
                  <Tag>输出上限 {formatCompact(capabilities.output_limit)}</Tag>
                </Tooltip>
              )}
              {capabilities.reasoning && <Tag color="geekblue">推理</Tag>}
              {capabilities.tool_call && <Tag color="green">工具调用</Tag>}
              {capabilities.attachment && <Tag color="orange">附件</Tag>}
              {capabilities.structured_output && <Tag>结构化输出</Tag>}
              {capabilities.input_modalities && (
                <Tag>输入 {capabilities.input_modalities.join('/')}</Tag>
              )}
            </div>
            {selected?.target_count != null && (
              <Typography.Text type="secondary">
                路由 {selected.target_count} 个目标，能力为共同下限
                {selected.limits_verified === false ? '（部分目标缺少元数据）' : ''}
              </Typography.Text>
            )}
          </div>
        )}
      </Space>
    </Card>
  )
}
