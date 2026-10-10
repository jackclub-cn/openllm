import {
  KeyOutlined,
  PlusOutlined,
  SearchOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import { Button, Input, Popconfirm, Select, Space, Tag } from 'antd'
import PageHeader from '../../components/PageHeader'
import type { ProviderHealth, ProviderStatus } from './types'

type ProviderToolbarProps = {
  providerSearch: string
  providerStatus: ProviderStatus
  providerHealth: ProviderHealth
  providerCount: number
  filteredCount: number
  testingAll: boolean
  testingAllKeys: boolean
  hasTestableKeys: boolean
  onSearchChange: (value: string) => void
  onStatusChange: (value: ProviderStatus) => void
  onHealthChange: (value: ProviderHealth) => void
  onTestAll: () => void
  onTestAllKeys: () => void
  onAdd: () => void
}

export default function ProviderToolbar({
  providerSearch,
  providerStatus,
  providerHealth,
  providerCount,
  filteredCount,
  testingAll,
  testingAllKeys,
  hasTestableKeys,
  onSearchChange,
  onStatusChange,
  onHealthChange,
  onTestAll,
  onTestAllKeys,
  onAdd,
}: ProviderToolbarProps) {
  return (
    <PageHeader
      title="提供商"
      description="管理 OpenAI、Anthropic、Ollama 及任意兼容 API"
      extra={
        <Space>
          <Input
            allowClear
            prefix={<SearchOutlined />}
            value={providerSearch}
            onChange={(event) => onSearchChange(event.target.value)}
            placeholder="搜索名称、地址、模型或密钥"
            style={{ width: 250 }}
          />
          <Select
            value={providerStatus}
            onChange={onStatusChange}
            style={{ width: 120 }}
            options={[
              { value: 'all', label: '全部状态' },
              { value: 'enabled', label: '已启用' },
              { value: 'disabled', label: '已停用' },
            ]}
          />
          <Select
            value={providerHealth}
            onChange={onHealthChange}
            style={{ width: 140 }}
            options={[
              { value: 'all', label: '全部健康' },
              { value: 'healthy', label: '提供商正常' },
              { value: 'failed', label: '提供商异常' },
              { value: 'untested', label: '提供商未检测' },
              { value: 'key_error', label: '密钥异常' },
              { value: 'key_untested', label: '密钥未检测' },
            ]}
          />
          <Tag>
            {filteredCount}/{providerCount}
          </Tag>
          <Button
            icon={<ThunderboltOutlined />}
            loading={testingAll}
            disabled={!providerCount}
            onClick={onTestAll}
          >
            测试全部
          </Button>
          <Popconfirm
            title="逐个探测所有启用密钥？"
            description="每个密钥都会向上游发送一个最小请求。"
            onConfirm={onTestAllKeys}
          >
            <Button
              icon={<KeyOutlined />}
              loading={testingAllKeys}
              disabled={!hasTestableKeys}
            >
              测试全部密钥
            </Button>
          </Popconfirm>
          <Button type="primary" icon={<PlusOutlined />} onClick={onAdd}>
            添加提供商
          </Button>
        </Space>
      }
    />
  )
}
