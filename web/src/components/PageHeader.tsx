import type { ReactNode } from 'react'
import { Space, Typography } from 'antd'

type Props = {
  title: string
  description: string
  extra?: ReactNode
}

export default function PageHeader({ title, description, extra }: Props) {
  return (
    <div className="page-header">
      <div>
        <Typography.Title level={3}>{title}</Typography.Title>
        <Typography.Text type="secondary">{description}</Typography.Text>
      </div>
      {extra && <Space wrap>{extra}</Space>}
    </div>
  )
}
