import { ReloadOutlined } from '@ant-design/icons'
import { Alert, Button } from 'antd'

type LoadErrorAlertProps = {
  title: string
  error: string
  loading: boolean
  onRetry: () => void
}

export default function LoadErrorAlert({
  title,
  error,
  loading,
  onRetry,
}: LoadErrorAlertProps) {
  if (!error) return null
  return (
    <Alert
      type="error"
      showIcon
      title={title}
      description={error}
      style={{ marginBottom: 16 }}
      action={
        <Button size="small" icon={<ReloadOutlined />} loading={loading} onClick={onRetry}>
          重试
        </Button>
      }
    />
  )
}
