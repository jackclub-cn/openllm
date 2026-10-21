import { Segmented, Space, Table, Tag, Tooltip, Typography } from 'antd'
import { useState } from 'react'
import type { ContextBreakdown } from '../api'
import { formatCompact, formatExact } from '../format'

const categoryLabels: Record<string, string> = {
  system: '系统提示',
  tools: '工具定义',
  tool_results: '工具结果',
  conversation: '会话内容',
  unattributed: '未归因',
}

const categoryColors: Record<string, string> = {
  system: '#722ed1',
  tools: '#13c2c2',
  tool_results: '#eb2f96',
  conversation: '#597ef7',
  unattributed: '#8c8c8c',
}

export default function ContextBreakdownPanel({
  context,
}: {
  context?: ContextBreakdown | null
}) {
  const [filter, setFilter] = useState('all')
  if (!context) {
    return <Typography.Text type="secondary">暂无上下文分类数据</Typography.Text>
  }
  const total = Math.max(1, context.input_tokens)
  const window = context.window_tokens && context.window_tokens > 0 ? context.window_tokens : null
  const usagePercent = window ? (context.input_tokens / window) * 100 : null
  const scale = Math.max(total, window || 0)
  const categories = context.categories.filter((item) => filter === 'all' || item.key === filter)
  const cells = Array.from({ length: 120 }, (_, index) => {
    const position = index * scale / 120
    let used = 0
    return context.categories.find((category) => {
      used += category.tokens
      return category.tokens > 0 && position < used
    })
  })

  return (
    <Space direction="vertical" size={12} style={{ width: '100%' }}>
      <Space size={8} wrap>
        <Typography.Text strong>{formatCompact(context.input_tokens)} tokens</Typography.Text>
        <Typography.Text type="secondary">
          {window ? ` / ${formatCompact(window)} ${context.window_source === 'route' ? '路由参考容量' : '窗口'}` : '模型窗口未知'}
        </Typography.Text>
        {context.input_limit != null && <Typography.Text type="secondary">输入上限 {formatCompact(context.input_limit)}</Typography.Text>}
        <Tag color="orange">分类估算</Tag>
        <Tag color={context.input_estimated ? 'default' : 'green'}>
          {context.input_estimated ? '输入估算' : '输入已确认'}
        </Tag>
        {usagePercent != null && (
          <Typography.Text type={usagePercent >= 90 ? 'danger' : 'secondary'}>
            已用 {usagePercent.toFixed(1)}%
          </Typography.Text>
        )}
      </Space>

      {context.unmetered_items > 0 && <Typography.Text type="secondary">多模态或未知内容 {context.unmetered_items} 项，用量未单独计量</Typography.Text>}
      <div className="context-token-grid" aria-label="上下文分类占比">
        {cells.map((category, index) => (
          <Tooltip key={index} title={category ? categoryLabels[category.key] : '剩余空间'}>
            <i style={{ background: category ? categoryColors[category.key] : undefined }} />
          </Tooltip>
        ))}
      </div>
      <div className="context-category-grid">
        {context.categories.map((category) => (
          <div key={category.key} className="context-category-item">
            <i style={{ background: categoryColors[category.key] || '#8c8c8c' }} />
            <span>{categoryLabels[category.key] || category.key}</span>
            <strong>{formatCompact(category.tokens)}</strong>
            <Typography.Text type="secondary">
              {((category.tokens / total) * 100).toFixed(1)}%
            </Typography.Text>
          </div>
        ))}
      </div>
      <div className="context-filter">
        <Segmented
          value={filter} onChange={setFilter}
          options={[{ value: 'all', label: '全部' }, ...context.categories.map((item) => ({
            value: item.key, label: categoryLabels[item.key] || item.key,
          }))]}
        />
      </div>
      <Table
        size="small" pagination={false} rowKey="key" dataSource={categories}
        columns={[
          { title: '分类', dataIndex: 'key', render: (key: string) => categoryLabels[key] || key },
          { title: '条目', dataIndex: 'items', render: (value?: number) => value ?? '-' },
          { title: 'Token', dataIndex: 'tokens', render: (value: number) => formatExact(value) },
        ]}
      />
    </Space>
  )
}
