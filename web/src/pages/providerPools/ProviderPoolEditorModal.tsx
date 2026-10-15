import { MinusCircleOutlined, PlusOutlined } from '@ant-design/icons'
import {
  Button,
  Divider,
  Form,
  Input,
  InputNumber,
  Modal,
  Select,
  Space,
  Switch,
  Tooltip,
  Typography,
} from 'antd'
import type { FormInstance } from 'antd'
import type { ApiKey, Provider, ProviderPool } from '../../api'
import {
  MAX_POOL_WINDOW_SECONDS,
  MIN_POOL_WINDOW_SECONDS,
  allocationPolicyOptions,
  type ProviderPoolFormValues,
} from './types'

type Props = {
  open: boolean
  editing?: ProviderPool
  saving: boolean
  form: FormInstance<ProviderPoolFormValues>
  providers: Provider[]
  apiKeys: ApiKey[]
  onCancel: () => void
  onSave: () => void
}

export default function ProviderPoolEditorModal({
  open,
  editing,
  saving,
  form,
  providers,
  apiKeys,
  onCancel,
  onSave,
}: Props) {
  const keyOptions = apiKeys.map((key) => ({
    value: key.id,
    label: `${key.name} · ${key.key_prefix}…${key.key_suffix}`,
    disabled: !key.enabled,
  }))

  return (
    <Modal
      title={editing ? '编辑配额池' : '新建配额池'}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      confirmLoading={saving}
      width={720}
      destroyOnHidden
    >
      <Form form={form} layout="vertical">
        <Form.Item
          name="provider_id"
          label="提供商"
          extra={editing ? '配额池创建后不能改挂到另一个提供商。' : undefined}
          rules={[{ required: true, message: '请选择提供商' }]}
        >
          <Select
            disabled={Boolean(editing)}
            showSearch
            optionFilterProp="label"
            placeholder="选择要声明容量的提供商"
            options={providers.map((provider) => ({
              value: provider.id,
              label: provider.name,
            }))}
          />
        </Form.Item>
        <Form.Item
          name="name"
          label="名称"
          rules={[{ required: true, message: '请输入名称' }]}
        >
          <Input placeholder="shared" maxLength={100} />
        </Form.Item>
        <Space size={24} align="start" wrap>
          <Form.Item
            name="window_seconds"
            label="滚动窗口（秒）"
            rules={[{ required: true, message: '请输入窗口秒数' }]}
          >
            <InputNumber
              min={MIN_POOL_WINDOW_SECONDS}
              max={MAX_POOL_WINDOW_SECONDS}
              style={{ width: 180 }}
            />
          </Form.Item>
          <Form.Item name="saturation_threshold_pct" label="饱和阈值（%）">
            <InputNumber min={1} max={100} style={{ width: 180 }} />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
        </Space>
        <Space size={24} align="start" wrap>
          <Form.Item name="request_limit" label="请求上限">
            <InputNumber min={1} style={{ width: 180 }} placeholder="不限" />
          </Form.Item>
          <Form.Item
            name="token_limit"
            label="Token 上限"
            dependencies={['request_limit']}
            rules={[
              ({ getFieldValue }) => ({
                validator(_, value: number | null | undefined) {
                  if (!getFieldValue('request_limit') && !value) {
                    return Promise.reject(
                      new Error('请求上限与 Token 上限至少填一个'),
                    )
                  }
                  return Promise.resolve()
                },
              }),
            ]}
          >
            <InputNumber min={1} style={{ width: 180 }} placeholder="不限" />
          </Form.Item>
        </Space>
        <Typography.Text type="secondary">
          达到饱和阈值前所有密钥都可借用空闲容量；之后按下列权重把容量分给各密钥。
        </Typography.Text>
        <Divider titlePlacement="start" plain>
          权重分配
        </Divider>
        <Form.List name="allocations">
          {(fields, { add, remove }) => (
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              {fields.map((field) => (
                <Space key={field.key} align="baseline" wrap>
                  <Form.Item
                    name={[field.name, 'api_key_id']}
                    rules={[
                      { required: true, message: '请选择访问密钥' },
                      ({ getFieldValue }) => ({
                        validator(_, value: number | undefined) {
                          const rows =
                            (getFieldValue('allocations') as Array<{
                              api_key_id?: number
                            }>) || []
                          const duplicates = rows.filter(
                            (row) => row?.api_key_id === value,
                          )
                          if (value && duplicates.length > 1) {
                            return Promise.reject(
                              new Error('同一密钥只能有一条分配'),
                            )
                          }
                          return Promise.resolve()
                        },
                      }),
                    ]}
                  >
                    <Select
                      showSearch
                      optionFilterProp="label"
                      placeholder="访问密钥"
                      style={{ width: 260 }}
                      options={keyOptions}
                    />
                  </Form.Item>
                  <Form.Item
                    name={[field.name, 'weight_pct']}
                    rules={[{ required: true, message: '请输入权重' }]}
                  >
                    <InputNumber
                      min={1}
                      max={100}
                      addonAfter="%"
                      style={{ width: 120 }}
                    />
                  </Form.Item>
                  <Form.Item
                    name={[field.name, 'policy']}
                    rules={[{ required: true, message: '请选择策略' }]}
                  >
                    <Select
                      style={{ width: 220 }}
                      options={allocationPolicyOptions}
                    />
                  </Form.Item>
                  <Tooltip title="删除这条分配">
                    <Button
                      type="text"
                      danger
                      aria-label="删除分配"
                      icon={<MinusCircleOutlined />}
                      onClick={() => remove(field.name)}
                    />
                  </Tooltip>
                </Space>
              ))}
              <Button
                type="dashed"
                icon={<PlusOutlined />}
                onClick={() => add({ weight_pct: 100, policy: 'hard' })}
              >
                添加分配
              </Button>
            </Space>
          )}
        </Form.List>
      </Form>
    </Modal>
  )
}
