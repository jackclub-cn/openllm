import { Alert, InputNumber, Modal, Space, Typography } from 'antd'

type UsageCleanupModalProps = {
  open: boolean
  days: number | null
  cleaning: boolean
  onDaysChange: (value: number | null) => void
  onCancel: () => void
  onConfirm: () => void
}

export default function UsageCleanupModal({
  open,
  days,
  cleaning,
  onDaysChange,
  onCancel,
  onConfirm,
}: UsageCleanupModalProps) {
  return (
    <Modal
      title="清理历史日志"
      open={open}
      onCancel={onCancel}
      onOk={onConfirm}
      confirmLoading={cleaning}
      okText="开始清理"
      okButtonProps={{ danger: true }}
      destroyOnHidden
    >
      <Alert
        type="warning"
        showIcon
        message="此操作不可撤销"
        description="将永久删除早于指定天数的用量记录，仪表盘的历史统计也会随之减少。"
        style={{ marginBottom: 16 }}
      />
      <Space>
        <Typography.Text>保留最近</Typography.Text>
        <InputNumber
          min={1}
          max={3650}
          value={days}
          onChange={onDaysChange}
          addonAfter="天"
        />
      </Space>
    </Modal>
  )
}
