import { ApiOutlined } from '@ant-design/icons'
import { Card, Modal, Space, Typography } from 'antd'
import { providerPresets } from '../../providerPresets'

type ProviderTemplateModalProps = {
  open: boolean
  onCancel: () => void
  onSelect: (key: string) => void
}

export default function ProviderTemplateModal({
  open,
  onCancel,
  onSelect,
}: ProviderTemplateModalProps) {
  return (
    <Modal
      title="选择提供商模板"
      open={open}
      onCancel={onCancel}
      footer={null}
      width={980}
      destroyOnHidden
    >
      <Typography.Paragraph type="secondary">
        先选择接近的模板，再填写 API Key。模板只预填协议、地址、前缀和推荐检测模型，所有字段仍可修改。
      </Typography.Paragraph>
      <div className="provider-template-groups">
        {Array.from(new Set(providerPresets.map((item) => item.group))).map((group) => (
          <section key={group}>
            <Typography.Title level={5}>{group}</Typography.Title>
            <div className="provider-template-grid">
              {providerPresets
                .filter((item) => item.group === group)
                .map((preset) => (
                  <Card
                    key={preset.key}
                    size="small"
                    hoverable
                    className="provider-template-card"
                    onClick={() => onSelect(preset.key)}
                  >
                    <Space align="start">
                      <div className="provider-template-icon">
                        <ApiOutlined />
                      </div>
                      <div>
                        <Typography.Text strong>{preset.label}</Typography.Text>
                        <Typography.Paragraph type="secondary" ellipsis={{ rows: 2 }}>
                          {preset.hint}
                        </Typography.Paragraph>
                      </div>
                    </Space>
                  </Card>
                ))}
            </div>
          </section>
        ))}
      </div>
    </Modal>
  )
}
