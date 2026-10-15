import {
  Checkbox,
  Form,
  Input,
  Modal,
  Select,
  Switch,
} from 'antd'
import type { FormInstance } from 'antd'
import type { Webhook } from '../../api'
import {
  webhookEventOptions,
  webhookKindOptions,
  type WebhookFormValues,
} from './types'

type Props = {
  open: boolean
  editing?: Webhook
  saving: boolean
  form: FormInstance<WebhookFormValues>
  onCancel: () => void
  onSave: () => void
}

export default function WebhookEditorModal({
  open,
  editing,
  saving,
  form,
  onCancel,
  onSave,
}: Props) {
  const kind = Form.useWatch('kind', form) ?? 'generic'
  const isChatChannel = kind !== 'generic'

  return (
    <Modal
      title={editing ? '编辑 Webhook' : '新建 Webhook'}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      confirmLoading={saving}
      width={620}
      destroyOnHidden
    >
      <Form form={form} layout="vertical">
        <Form.Item
          name="name"
          label="名称"
          rules={[{ required: true, message: '请输入名称' }]}
        >
          <Input placeholder="审计与告警" maxLength={120} />
        </Form.Item>
        <Form.Item
          name="url"
          label="投递地址"
          rules={[
            { required: true, message: '请输入投递地址' },
            { type: 'url', message: '请输入完整的 HTTP 或 HTTPS 地址' },
          ]}
        >
          <Input placeholder="https://example.com/openllm/events" />
        </Form.Item>
        <Form.Item
          name="secret"
          label="签名密钥"
          extra={editing?.secret_set ? '留空则继续使用当前密钥。' : '填写后，请求会附带 HMAC-SHA256 签名。'}
        >
          <Input.Password placeholder={editing?.secret_set ? '保持当前密钥' : '可选'} />
        </Form.Item>
        {editing?.secret_set && (
          <Form.Item name="clear_secret" valuePropName="checked">
            <Checkbox>清除现有签名密钥</Checkbox>
          </Form.Item>
        )}
        <Form.Item
          name="headers"
          label="自定义请求头"
          extra='JSON 对象，例如 {"Authorization":"Bearer ..."}。网关事件与签名头不可覆盖。'
          rules={[
            {
              validator: (_, value: string) => {
                if (!value?.trim()) return Promise.resolve()
                try {
                  const parsed = JSON.parse(value)
                  if (
                    parsed &&
                    typeof parsed === 'object' &&
                    !Array.isArray(parsed) &&
                    Object.values(parsed).every((item) => typeof item === 'string')
                  ) {
                    return Promise.resolve()
                  }
                } catch {
                  // Use the same error below for malformed JSON and wrong shapes.
                }
                return Promise.reject(new Error('请输入值为字符串的 JSON 对象'))
              },
            },
          ]}
        >
          <Input.TextArea rows={3} placeholder='{"Authorization":"Bearer ..."}' />
        </Form.Item>
        <Form.Item
          name="kind"
          label="投递格式"
          extra="选择 generic 会发送网关事件信封；选择聊天平台会改发一条可读消息。"
        >
          <Select options={webhookKindOptions} />
        </Form.Item>
        {isChatChannel && (
          <Form.Item
            name="body_fields"
            label="附加字段"
            extra={
              kind === 'telegram'
                ? 'JSON 对象。Telegram 必须提供 chat_id，例如 {"chat_id":"-1001234567890"}。'
                : 'JSON 对象，会被合并进消息体，例如 {"channel":"#ops"}。'
            }
            rules={[
              {
                validator: (_, value: string) => {
                  let parsed: unknown = {}
                  if (value?.trim()) {
                    try {
                      parsed = JSON.parse(value)
                    } catch {
                      return Promise.reject(new Error('请输入 JSON 对象'))
                    }
                  }
                  if (
                    typeof parsed !== 'object' ||
                    parsed === null ||
                    Array.isArray(parsed)
                  ) {
                    return Promise.reject(new Error('请输入 JSON 对象'))
                  }
                  if (kind === 'telegram') {
                    const chat = (parsed as Record<string, unknown>).chat_id
                    const text =
                      typeof chat === 'number'
                        ? String(chat)
                        : typeof chat === 'string'
                          ? chat.trim()
                          : ''
                    if (!text) {
                      return Promise.reject(new Error('Telegram 需要填写 chat_id'))
                    }
                  }
                  return Promise.resolve()
                },
              },
            ]}
          >
            <Input.TextArea rows={2} placeholder='{"channel":"#ops"}' />
          </Form.Item>
        )}
        <Form.Item
          name="event_types"
          label="订阅事件"
          rules={[{ required: true, message: '请选择至少一个事件' }]}
        >
          <Select mode="multiple" options={webhookEventOptions} />
        </Form.Item>
        <Form.Item name="enabled" label="启用" valuePropName="checked">
          <Switch />
        </Form.Item>
      </Form>
    </Modal>
  )
}
