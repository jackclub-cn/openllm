import { ExperimentOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons'
import { Button, Input, Select, Space, Tag } from 'antd'
import type { GatewayRoute } from '../../api'
import PageHeader from '../../components/PageHeader'
import { strategyLabels } from './types'

type RoutesToolbarProps = {
  search: string
  status: 'all' | 'enabled' | 'disabled'
  strategy: 'all' | GatewayRoute['strategy']
  routeCount: number
  filteredCount: number
  onSearchChange: (value: string) => void
  onStatusChange: (value: 'all' | 'enabled' | 'disabled') => void
  onStrategyChange: (value: 'all' | GatewayRoute['strategy']) => void
  onDiagnose: () => void
  onCreate: () => void
}

export default function RoutesToolbar({
  search,
  status,
  strategy,
  routeCount,
  filteredCount,
  onSearchChange,
  onStatusChange,
  onStrategyChange,
  onDiagnose,
  onCreate,
}: RoutesToolbarProps) {
  return (
    <PageHeader
      title="模型路由"
      description="按模型通配符选择上游，并配置故障切换与负载均衡"
      extra={
        <Space>
          <Input
            allowClear
            prefix={<SearchOutlined />}
            value={search}
            onChange={(event) => onSearchChange(event.target.value)}
            placeholder="搜索路由、模型或上游目标"
            style={{ width: 250 }}
          />
          <Select
            value={status}
            onChange={onStatusChange}
            style={{ width: 120 }}
            options={[
              { value: 'all', label: '全部状态' },
              { value: 'enabled', label: '已启用' },
              { value: 'disabled', label: '已停用' },
            ]}
          />
          <Select
            value={strategy}
            onChange={onStrategyChange}
            style={{ width: 150 }}
            options={[
              { value: 'all', label: '全部策略' },
              ...Object.entries(strategyLabels).map(([value, label]) => ({
                value,
                label,
              })),
            ]}
          />
          <Tag>
            {filteredCount}/{routeCount}
          </Tag>
          <Button icon={<ExperimentOutlined />} onClick={onDiagnose}>
            路由诊断
          </Button>
          <Button type="primary" icon={<PlusOutlined />} onClick={onCreate}>
            创建路由
          </Button>
        </Space>
      }
    />
  )
}
