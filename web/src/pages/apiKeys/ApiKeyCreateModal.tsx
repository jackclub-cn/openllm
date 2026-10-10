import { CopyOutlined } from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  DatePicker,
  Form,
  Input,
  InputNumber,
  Modal,
  Space,
  type FormInstance,
} from 'antd'
import type { ApiKeyForm } from './types'
import ApiKeyRoutingFields from './ApiKeyRoutingFields'

type ApiKeyCreateModalProps = {
  open: boolean
  createdKey: string
  saving: boolean
  form: FormInstance<ApiKeyForm>
  onCancel: () => void
  onCreate: () => void
}

export default function ApiKeyCreateModal({
  open,
  createdKey,
  saving,
  form,
  onCancel,
  onCreate,
}: ApiKeyCreateModalProps) {
  const { message } = App.useApp()

  return (
    <Modal
      title={createdKey ? '密钥已生成' : '创建访问密钥'}
      open={open}
      onCancel={onCancel}
      footer={
        createdKey ? (
          <Button type="primary" onClick={onCancel}>
            完成
          </Button>
        ) : undefined
      }
      onOk={createdKey ? onCancel : onCreate}
      confirmLoading={saving}
      width={560}
      destroyOnHidden
    >
      {createdKey ? (
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          <Alert type="warning" showIcon message="请立即保存，此密钥之后无法再次查看。" />
          <Input
            value={createdKey}
            readOnly
            addonAfter={
              <Button
                type="text"
                icon={<CopyOutlined />}
                onClick={() => {
                  void navigator.clipboard.writeText(createdKey)
                  message.success('已复制')
                }}
              />
            }
          />
        </Space>
      ) : (
        <Form form={form} layout="vertical">
          <Form.Item
            name="name"
            label="密钥名称"
            rules={[{ required: true, message: '请输入名称' }]}
          >
            <Input placeholder="例如 应用服务器" />
          </Form.Item>
          <div className="form-grid">
            <Form.Item
              name="daily_token_limit"
              label="每日 token 上限"
              extra="留空或 0 表示不限制。"
            >
              <InputNumber min={0} precision={0} style={{ width: '100%' }} />
            </Form.Item>
            <Form.Item
              name="daily_cost_limit_usd"
              label="每日费用上限（USD）"
              extra="留空或 0 表示不限制。"
            >
              <InputNumber min={0} precision={4} style={{ width: '100%' }} />
            </Form.Item>
            <Form.Item
              name="requests_per_minute"
              label="每分钟请求上限"
              extra="按 UTC 自然分钟统计，留空或 0 表示不限制。"
            >
              <InputNumber min={0} precision={0} style={{ width: '100%' }} />
            </Form.Item>
            <Form.Item
              name="max_concurrency"
              label="最大并发请求数"
              extra="统计已进入上游调用的请求，留空或 0 表示不限制。"
            >
              <InputNumber min={0} precision={0} style={{ width: '100%' }} />
            </Form.Item>
          </div>
          <Form.Item
            name="allowed_models_text"
            label="允许调用的模型"
            extra="每行一个模型名或通配符，例如 gpt-*。留空表示允许全部模型。"
          >
            <Input.TextArea rows={3} placeholder={'gpt-*\nclaude-sonnet-*'} />
          </Form.Item>
          <Form.Item name="expires_at" label="到期时间" extra="留空表示永不过期。">
            <DatePicker showTime style={{ width: '100%' }} />
          </Form.Item>
          <ApiKeyRoutingFields />
        </Form>
      )}
    </Modal>
  )
}
