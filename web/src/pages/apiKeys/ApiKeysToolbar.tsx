import { PlusOutlined, SearchOutlined } from '@ant-design/icons'
import { Alert, Button, Input, Select, Space, Tag } from 'antd'
import PageHeader from '../../components/PageHeader'

type ApiKeyStatus = 'all' | 'enabled' | 'disabled' | 'expired'

type ApiKeysToolbarProps = {
  search: string
  status: ApiKeyStatus
  itemCount: number
  filteredCount: number
  onSearchChange: (value: string) => void
  onStatusChange: (value: ApiKeyStatus) => void
  onCreate: () => void
}

export default function ApiKeysToolbar({
  search,
  status,
  itemCount,
  filteredCount,
  onSearchChange,
  onStatusChange,
  onCreate,
}: ApiKeysToolbarProps) {
  return (
    <>
      <PageHeader
        title="访问密钥"
        description="为调用方签发网关密钥；密钥仅在创建时显示一次"
        extra={
          <Space>
            <Input
              allowClear
              prefix={<SearchOutlined />}
              value={search}
              onChange={(event) => onSearchChange(event.target.value)}
              placeholder="搜索名称、密钥或模型权限"
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
                { value: 'expired', label: '已过期' },
              ]}
            />
            <Tag>
              {filteredCount}/{itemCount}
            </Tag>
            <Button type="primary" icon={<PlusOutlined />} onClick={onCreate}>
              创建密钥
            </Button>
          </Space>
        }
      />
      <Alert
        className="page-alert"
        type="info"
        showIcon
        message="未创建任何密钥时，网关默认允许匿名访问。创建首个密钥后，所有 /v1 请求必须携带 Bearer Token。"
      />
    </>
  )
}
