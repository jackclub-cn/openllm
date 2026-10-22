import { useEffect, useRef } from 'react'
import { App, type FormInstance } from 'antd'
import { useBeforeUnload, useBlocker } from 'react-router-dom'

export function useFormCloseGuard<Values extends object>(
  form: FormInstance<Values>,
  open: boolean,
  saving: boolean,
) {
  const { message, modal } = App.useApp()
  const baseline = useRef<string | undefined>(undefined)
  const confirmation = useRef<ReturnType<typeof modal.confirm> | undefined>(undefined)

  useEffect(() => {
    baseline.current = open ? JSON.stringify(form.getFieldsValue(true)) : undefined
    return () => {
      confirmation.current?.destroy()
      confirmation.current = undefined
    }
  }, [form, open])

  // Read the form store directly so programmatic changes (including target
  // reordering) and edits reverted to their original values are both covered.
  const hasChanges = () =>
    baseline.current !== undefined &&
    JSON.stringify(form.getFieldsValue(true)) !== baseline.current

  const blocker = useBlocker(() => open && (saving || hasChanges()))

  useBeforeUnload((event) => {
    if (!open || (!saving && !hasChanges())) return
    event.preventDefault()
    event.returnValue = ''
  })

  useEffect(() => {
    if (blocker.state !== 'blocked') return
    if (saving) {
      message.info('正在保存，请稍候')
      blocker.reset()
      return
    }
    const dialog = modal.confirm({
      title: '放弃修改并离开？',
      content: '当前有未保存的修改，离开后将丢失。',
      okText: '放弃修改并离开',
      cancelText: '继续编辑',
      okButtonProps: { danger: true },
      onOk: () => blocker.proceed(),
      onCancel: () => blocker.reset(),
    })
    return () => dialog.destroy()
  }, [blocker, message, modal, saving])

  return (onClose: () => void) => {
    if (saving || confirmation.current) return
    if (!hasChanges()) {
      onClose()
      return
    }
    confirmation.current = modal.confirm({
      title: '放弃未保存的修改？',
      content: '当前修改尚未保存，放弃后将丢失。',
      okText: '放弃修改',
      cancelText: '继续编辑',
      okButtonProps: { danger: true },
      onOk: onClose,
      afterClose: () => {
        confirmation.current = undefined
      },
    })
  }
}
