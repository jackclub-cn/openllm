import {
  DatePicker,
  Form,
  Input,
  InputNumber,
  Modal,
  type FormInstance,
} from 'antd'
import type { ApiKey } from '../../api'
import type { ApiKeyForm } from './types'
import ApiKeyRoutingFields from './ApiKeyRoutingFields'

type ApiKeyLimitsModalProps = {
  open: boolean
  editing?: ApiKey
  saving: boolean
  form: FormInstance<ApiKeyForm>
  onCancel: () => void
  onSave: () => void
}

export default function ApiKeyLimitsModal({
  open,
  editing,
  saving,
  form,
  onCancel,
  onSave,
}: ApiKeyLimitsModalProps) {
  return (
    <Modal
      title={editing ? `访问策略 · ${editing.name}` : '访问策略'}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      confirmLoading={saving}
      width={520}
      destroyOnHidden
    >
      <Form form={form} layout="vertical">
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
          extra="费用为预估值；无定价请求不会计入费用上限。"
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
        <ApiKeyRoutingFields />
      </Form>
    </Modal>
  )
}
