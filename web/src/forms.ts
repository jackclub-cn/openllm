import type { FormInstance } from 'antd'

/**
 * Validate a form without letting a failed validation escape as a rejected
 * promise.
 *
 * antd's `validateFields()` rejects as soon as any rule fails, and the console
 * calls it from `onOk={() => void save()}`. Awaiting it directly therefore left
 * an unhandled rejection behind every time an operator clicked 确定 on an
 * incomplete form, even though antd had already rendered the field errors.
 * Returning `undefined` lets the caller simply stop.
 */
export async function validateOrUndefined<Values extends object>(
  form: FormInstance<Values>,
): Promise<Values | undefined> {
  try {
    return await form.validateFields()
  } catch {
    return undefined
  }
}
