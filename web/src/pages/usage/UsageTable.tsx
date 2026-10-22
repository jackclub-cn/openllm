import { Card, Table } from 'antd'
import { useState } from 'react'
import type { UsageAttempt, UsageLog } from '../../api'
import { usageTableColumns } from './usageTableColumns'

type UsageTableProps = {
  items: UsageLog[]
  loading: boolean
  page: number
  pageSize: number
  total: number
  onPageChange: (page: number, pageSize: number) => void
  onOpenDetail: (record: UsageLog) => void
}

export default function UsageTable({
  items,
  loading,
  page,
  pageSize,
  total,
  onPageChange,
  onOpenDetail,
}: UsageTableProps) {
  const [collapsedRequests, setCollapsedRequests] = useState<Set<string>>(() => new Set())

  return (
    <Card bordered={false} className="table-card">
      <Table<UsageLog | UsageAttempt>
        rowKey={(record) =>
          'attempt_index' in record
            ? `${record.request_id}:attempt:${record.attempt_index}`
            : record.request_id
        }
        loading={loading}
        dataSource={items}
        expandable={{
          expandIconColumnIndex: 0,
          expandedRowKeys: items
            .filter((record) => record.children?.length && !collapsedRequests.has(record.request_id))
            .map((record) => record.request_id),
          onExpand: (expanded, record) => {
            setCollapsedRequests((current) => {
              const next = new Set(current)
              if (expanded) next.delete(record.request_id)
              else next.add(record.request_id)
              return next
            })
          },
        }}
        scroll={{ x: 1785 }}
        onRow={(record) => ({
          onClick: () => {
            if (!('attempt_index' in record)) onOpenDetail(record)
          },
          style: { cursor: 'attempt_index' in record ? 'default' : 'pointer' },
        })}
        pagination={{
          current: page,
          pageSize,
          total,
          showSizeChanger: true,
          showTotal: (value) => `共 ${value} 条`,
          onChange: onPageChange,
        }}
        columns={usageTableColumns(onOpenDetail)}
      />
    </Card>
  )
}
