import { useState } from 'react'
import { DeleteOutlined, HolderOutlined, PlusOutlined } from '@ant-design/icons'
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
  Tooltip,
  Typography,
  type FormInstance,
} from 'antd'
import type { GatewayRoute, Provider, RouteTarget } from '../../api'
import { strategyLabels, type RouteFormValues } from './types'

type RouteEditorModalProps = {
  open: boolean
  editing?: GatewayRoute
  saving: boolean
  providers: Provider[]
  form: FormInstance<RouteFormValues>
  onCancel: () => void
  onSave: () => void
}

function providerModels(providers: Provider[], providerId?: number) {
  return providers.find((provider) => provider.id === providerId)?.models || []
}

export default function RouteEditorModal({
  open,
  editing,
  saving,
  providers,
  form,
  onCancel,
  onSave,
}: RouteEditorModalProps) {
  const watchedTargets = Form.useWatch('targets', form)
  const [draggedTargetIndex, setDraggedTargetIndex] = useState<number>()
  const [dragOverTargetIndex, setDragOverTargetIndex] = useState<number>()

  const reorderTargets = (from: number, to: number) => {
    if (from === to) return
    const targets = [...((form.getFieldValue('targets') as RouteTarget[] | undefined) ?? [])]
    const [moved] = targets.splice(from, 1)
    if (!moved) return
    targets.splice(to, 0, moved)
    form.setFieldValue(
      'targets',
      targets.map((target, index) => ({ ...target, priority: index })),
    )
  }

  return (
    <Modal
      title={editing ? '编辑路由' : '创建路由'}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      confirmLoading={saving}
      width={860}
      destroyOnHidden
    >
      <Form form={form} layout="vertical">
        <div className="form-grid">
          <Form.Item
            name="name"
            label="路由名称"
            rules={[{ required: true, message: '请输入名称' }]}
          >
            <Input placeholder="默认聊天模型" />
          </Form.Item>
          <Form.Item
            name="model_pattern"
            label="模型匹配"
            extra="支持 * 和 ? 通配符，例如 gpt-* 或 claude-*。"
            rules={[{ required: true, message: '请输入模型匹配规则' }]}
          >
            <Input placeholder="gpt-*" />
          </Form.Item>
          <Form.Item name="strategy" label="调度策略" rules={[{ required: true }]}>
            <Select
              options={Object.entries(strategyLabels).map(([value, label]) => ({
                value,
                label,
              }))}
            />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
        </div>
        <Typography.Title level={5}>上游目标</Typography.Title>
        <Form.List name="targets">
          {(fields, { add, remove: removeTarget }) => (
            <Space direction="vertical" size={12} style={{ width: '100%' }}>
              {fields.map((field) => (
                <div
                  className={[
                    'target-row',
                    draggedTargetIndex === field.name ? 'target-row-dragging' : '',
                    dragOverTargetIndex === field.name ? 'target-row-drag-over' : '',
                  ]
                    .filter(Boolean)
                    .join(' ')}
                  key={field.key}
                  onDragOver={(event) => {
                    if (draggedTargetIndex === undefined) return
                    event.preventDefault()
                    event.dataTransfer.dropEffect = 'move'
                    setDragOverTargetIndex(field.name)
                  }}
                  onDragLeave={(event) => {
                    const relatedTarget = event.relatedTarget
                    if (
                      !(relatedTarget instanceof Node) ||
                      !event.currentTarget.contains(relatedTarget)
                    ) {
                      setDragOverTargetIndex((current) =>
                        current === field.name ? undefined : current,
                      )
                    }
                  }}
                  onDrop={(event) => {
                    event.preventDefault()
                    if (draggedTargetIndex !== undefined) {
                      reorderTargets(draggedTargetIndex, field.name)
                    }
                    setDraggedTargetIndex(undefined)
                    setDragOverTargetIndex(undefined)
                  }}
                >
                  <Tooltip title="拖拽调整顺序，也可使用方向键">
                    <span
                      aria-label="拖拽调整顺序"
                      className="target-drag-handle"
                      draggable
                      role="button"
                      tabIndex={0}
                      onDragStart={(event) => {
                        setDraggedTargetIndex(field.name)
                        event.dataTransfer.effectAllowed = 'move'
                        event.dataTransfer.setData('text/plain', String(field.name))
                      }}
                      onKeyDown={(event) => {
                        if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
                        event.preventDefault()
                        const to = event.key === 'ArrowUp' ? field.name - 1 : field.name + 1
                        if (to < 0 || to >= fields.length) return
                        reorderTargets(field.name, to)
                        setDragOverTargetIndex(to)
                      }}
                      onDragEnd={() => {
                        setDraggedTargetIndex(undefined)
                        setDragOverTargetIndex(undefined)
                      }}
                    >
                      <HolderOutlined />
                    </span>
                  </Tooltip>
                  <Form.Item
                    {...field}
                    name={[field.name, 'provider_id']}
                    rules={[{ required: true, message: '选择提供商' }]}
                  >
                    <Select
                      placeholder="提供商"
                      onChange={(value, previous) => {
                        if (value !== previous) {
                          form.setFieldValue(
                            ['targets', field.name, 'upstream_model'],
                            undefined,
                          )
                        }
                      }}
                      options={providers
                        .filter((provider) => provider.enabled)
                        .map((provider) => ({
                          value: provider.id,
                          label: provider.name,
                        }))}
                    />
                  </Form.Item>
                  <Form.Item
                    {...field}
                    name={[field.name, 'upstream_model']}
                    rules={[{ required: true, message: '填写上游模型' }]}
                  >
                    <AutoComplete
                      placeholder="上游模型名称"
                      options={providerModels(
                        providers,
                        watchedTargets?.[field.name]?.provider_id,
                      ).map((value) => ({ value }))}
                      filterOption={(input, option) =>
                        String(option?.value || '')
                          .toLowerCase()
                          .includes(input.toLowerCase())
                      }
                    />
                  </Form.Item>
                  <Form.Item {...field} name={[field.name, 'priority']}>
                    <InputNumber min={0} placeholder="优先级" />
                  </Form.Item>
                  <Form.Item {...field} name={[field.name, 'weight']}>
                    <InputNumber min={1} placeholder="权重" />
                  </Form.Item>
                  <Form.Item {...field} name={[field.name, 'enabled']} valuePropName="checked">
                    <Switch checkedChildren="启用" unCheckedChildren="停用" />
                  </Form.Item>
                  <Button
                    type="text"
                    danger
                    aria-label="删除目标"
                    icon={<DeleteOutlined />}
                    onClick={() => removeTarget(field.name)}
                  />
                </div>
              ))}
              <Button
                type="dashed"
                block
                icon={<PlusOutlined />}
                onClick={() => add({ weight: 100, priority: 0, enabled: true })}
              >
                添加上游目标
              </Button>
            </Space>
          )}
        </Form.List>
      </Form>
    </Modal>
  )
}
