import { AppstoreOutlined, DeleteOutlined, PlusOutlined } from '@ant-design/icons'
import {
  AutoComplete,
  Button,
  Form,
  Input,
  InputNumber,
  Modal,
  Select,
  Space,
  Switch,
  Typography,
  type FormInstance,
} from 'antd'
import type { Provider } from '../../api'
import { providerPresets } from '../../providerPresets'
import { providerLabels, type ProviderForm } from './types'

type ProviderEditorModalProps = {
  open: boolean
  editing?: Provider
  selectedPreset?: (typeof providerPresets)[number]
  saving: boolean
  form: FormInstance<ProviderForm>
  apiKeyRows: NonNullable<ProviderForm['api_keys']>
  onCancel: () => void
  onSave: () => void
  onChangeTemplate: () => void
}

export default function ProviderEditorModal({
  open,
  editing,
  selectedPreset,
  saving,
  form,
  apiKeyRows,
  onCancel,
  onSave,
  onChangeTemplate,
}: ProviderEditorModalProps) {
  return (
    <Modal
      title={editing ? '编辑提供商' : '添加提供商'}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      confirmLoading={saving}
      width={680}
      destroyOnHidden
    >
      <Form form={form} layout="vertical" className="modal-form">
        {!editing && selectedPreset && (
          <div className="provider-template-selected">
            <Space>
              <AppstoreOutlined />
              <div>
                <Typography.Text strong>{selectedPreset.label}</Typography.Text>
                <div>
                  <Typography.Text type="secondary">{selectedPreset.hint}</Typography.Text>
                </div>
              </div>
            </Space>
            <Button onClick={onChangeTemplate}>更换模板</Button>
          </div>
        )}
        <div className="form-grid">
          <Form.Item name="name" label="名称" rules={[{ required: true, message: '请输入名称' }]}>
            <Input placeholder="例如 OpenAI" />
          </Form.Item>
          <Form.Item name="provider_type" label="协议类型" rules={[{ required: true }]}>
            <Select
              options={Object.entries(providerLabels).map(([value, label]) => ({
                value,
                label,
              }))}
            />
          </Form.Item>
        </div>
        <Form.Item
          name="base_url"
          label="API 基础地址"
          rules={[{ required: true, message: '请输入 API 地址' }]}
        >
          <Input placeholder="https://api.openai.com/v1" />
        </Form.Item>
        <Form.Item
          name="model_prefix"
          label="模型前缀"
          extra="用于命名空间和自动路由，例如 openai/、local/。留空则不添加前缀。"
        >
          <Input placeholder="openai/" />
        </Form.Item>
        <Form.Item
          label="API Keys"
          extra="支持配置多个上游密钥；请求会轮转使用，遇到 401/403 时自动尝试下一把。已有密钥留空即保留原值。"
        >
          <Form.List name="api_keys">
            {(fields, { add, remove }) => (
              <Space direction="vertical" size={8} style={{ width: '100%' }}>
                {fields.map((field, index) => (
                  <Space key={field.key} align="start" style={{ width: '100%' }}>
                    <Form.Item {...field} name={[field.name, 'id']} hidden>
                      <Input />
                    </Form.Item>
                    <Form.Item
                      {...field}
                      name={[field.name, 'name']}
                      style={{ marginBottom: 0, width: 150 }}
                    >
                      <Input placeholder={`Key ${index + 1}`} />
                    </Form.Item>
                    <Form.Item
                      {...field}
                      name={[field.name, 'api_key']}
                      style={{ marginBottom: 0, width: 260 }}
                    >
                      <Input.Password
                        placeholder={
                          apiKeyRows[index]?.id
                            ? `留空保留${apiKeyRows[index]?.api_key_suffix ? ` (****${apiKeyRows[index].api_key_suffix})` : ''}`
                            : 'sk-...'
                        }
                        autoComplete="new-password"
                      />
                    </Form.Item>
                    <Form.Item
                      {...field}
                      name={[field.name, 'enabled']}
                      valuePropName="checked"
                      style={{ marginBottom: 0 }}
                    >
                      <Switch checkedChildren="启用" unCheckedChildren="停用" />
                    </Form.Item>
                    <Button
                      type="text"
                      danger
                      icon={<DeleteOutlined />}
                      onClick={() => remove(field.name)}
                    />
                  </Space>
                ))}
                <Button
                  type="dashed"
                  icon={<PlusOutlined />}
                  onClick={() => add({ name: `Key ${fields.length + 1}`, enabled: true })}
                  block
                >
                  添加密钥
                </Button>
              </Space>
            )}
          </Form.List>
        </Form.Item>
        <Form.Item
          name="auto_sync_models"
          label="保存后自动从上游同步模型"
          valuePropName="checked"
        >
          <Switch />
        </Form.Item>
        <Form.Item
          name="health_check_interval_minutes"
          label="自动健康检查间隔（分钟）"
          extra="0 或留空表示关闭；启用后后台会按间隔检测并更新健康状态。"
        >
          <InputNumber min={0} precision={0} style={{ width: '100%' }} />
        </Form.Item>
        <Form.Item
          name="health_check_model"
          label="健康检查模型"
          extra="留空时使用同步模型列表中的第一个启用模型；也可手动填写仅供检测使用的模型名。"
        >
          <AutoComplete
            allowClear
            options={(editing?.models || []).map((model) => ({ value: model }))}
            placeholder="例如 gpt-5.4-mini"
          />
        </Form.Item>
        <Form.Item
          name="models_sync_interval_minutes"
          label="自动模型同步间隔（分钟）"
          extra="0 或留空表示关闭；失败也会记录尝试时间，避免每分钟重复请求。"
        >
          <InputNumber min={0} precision={0} style={{ width: '100%' }} />
        </Form.Item>
        <div className="form-grid">
          <Form.Item
            name="timeout_seconds"
            label="请求超时（秒）"
            extra="0 或留空沿用全局空闲超时；设置后限制单次上游请求总时长，最长 3600 秒。"
          >
            <InputNumber min={0} max={3600} precision={0} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item
            name="configured_cooldown_seconds"
            label="失败冷却基准（秒）"
            extra="0 或留空沿用内置策略；连续失败会递增，并至少遵循上游 Retry-After，最长 3600 秒。"
          >
            <InputNumber min={0} max={3600} precision={0} style={{ width: '100%' }} />
          </Form.Item>
        </div>
        <Form.Item
          name="modelsText"
          label="支持的模型"
          extra="每行一个模型名。点击列表中的同步按钮可直接覆盖为上游最新模型。"
        >
          <Input.TextArea rows={4} placeholder={'gpt-4.1\ngpt-4.1-mini'} />
        </Form.Item>
        <Form.Item name="headersText" label="附加请求头（JSON）">
          <Input.TextArea
            rows={4}
            className="code-input"
            placeholder={'{"X-Organization": "team-a"}'}
          />
        </Form.Item>
      </Form>
    </Modal>
  )
}
