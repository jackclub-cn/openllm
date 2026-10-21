import { ArrowLeftOutlined, LockOutlined } from '@ant-design/icons'
import { useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Form,
  Input,
  Space,
  Typography,
} from 'antd'
import { ApiError, formatError } from '../api'

type Props = {
  version?: string
  updating: boolean
  initialError?: string
  onSubmit: (value: string) => Promise<void>
  onBack?: () => void
}

type LoginValues = {
  token: string
}

export default function AdminLogin({
  version,
  updating,
  initialError,
  onSubmit,
  onBack,
}: Props) {
  const [form] = Form.useForm<LoginValues>()
  const [error, setError] = useState(initialError ?? '')
  const [submitting, setSubmitting] = useState(false)

  const submit = async ({ token }: LoginValues) => {
    setSubmitting(true)
    setError('')
    try {
      await onSubmit(token.trim())
    } catch (reason) {
      setError(
        reason instanceof ApiError && reason.status === 401
          ? '管理令牌无效，请重新输入。'
          : `无法验证管理令牌：${formatError(reason)}`
      )
      form.getFieldInstance('token')?.focus()
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <main className="auth-screen">
      <Card className="auth-card" bordered={false}>
        <div className="auth-brand">
          <div className="brand-mark">O</div>
          <div>
            <div className="brand-name">OpenLLM</div>
            <div className="brand-caption">Gateway Console</div>
          </div>
        </div>

        <Space direction="vertical" size={6} className="auth-heading">
          <Typography.Title level={3}>
            {updating ? '更新管理令牌' : '管理登录'}
          </Typography.Title>
          <Typography.Text type="secondary">
            输入服务启动时设置的 OPENLLM_ADMIN_TOKEN。
          </Typography.Text>
        </Space>

        {error && (
          <Alert
            className="auth-alert"
            type="error"
            showIcon
            message={error}
            closable
            onClose={() => setError('')}
          />
        )}

        <Form form={form} layout="vertical" onFinish={(values) => void submit(values)}>
          <Form.Item
            label="OPENLLM_ADMIN_TOKEN"
            name="token"
            rules={[{ required: true, message: '请输入管理令牌' }]}
          >
            <Input.Password
              autoFocus
              autoComplete="current-password"
              prefix={<LockOutlined />}
              placeholder="输入管理令牌"
            />
          </Form.Item>
          <Button type="primary" htmlType="submit" block loading={submitting}>
            {updating ? '验证并更新' : '进入控制台'}
          </Button>
        </Form>

        <Typography.Paragraph type="secondary" className="auth-hint">
          令牌只保存在当前浏览器，不会写入数据库。清除浏览器站点数据即可退出管理登录。
        </Typography.Paragraph>

        <div className="auth-footer">
          {onBack && (
            <Button type="text" icon={<ArrowLeftOutlined />} onClick={onBack}>
              返回控制台
            </Button>
          )}
          {version && <Typography.Text type="secondary">v{version}</Typography.Text>}
        </div>
      </Card>
    </main>
  )
}
