const ADMIN_TOKEN_KEY = 'openllm-admin-token'

export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

export function getAdminToken() {
  return localStorage.getItem(ADMIN_TOKEN_KEY) || ''
}

export function setAdminToken(value: string) {
  if (value.trim()) localStorage.setItem(ADMIN_TOKEN_KEY, value.trim())
  else localStorage.removeItem(ADMIN_TOKEN_KEY)
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (init?.body && !headers.has('Content-Type'))
    headers.set('Content-Type', 'application/json')
  const adminToken = getAdminToken()
  if (adminToken) headers.set('x-admin-token', adminToken)

  const response = await fetch(path, { ...init, headers })
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`
    try {
      const body = await response.json()
      message = body?.error?.message || message
    } catch {
      // Keep the HTTP status when the body is not JSON.
    }
    throw new ApiError(message, response.status)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

export type DownloadedFile = {
  blob: Blob
  /// The server capped the export at its row limit; the file is incomplete.
  truncated: boolean
}

async function requestBlob(path: string): Promise<DownloadedFile> {
  const headers = new Headers()
  const adminToken = getAdminToken()
  if (adminToken) headers.set('x-admin-token', adminToken)
  const response = await fetch(path, { headers })
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`
    try {
      const body = await response.json()
      message = body?.error?.message || message
    } catch {
      // Keep the HTTP status when the body is not JSON.
    }
    throw new ApiError(message, response.status)
  }
  return {
    blob: await response.blob(),
    truncated: response.headers.get('x-openllm-export-truncated') === 'true',
  }
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, {
      method: 'POST',
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  put: <T>(path: string, body: unknown) =>
    request<T>(path, { method: 'PUT', body: JSON.stringify(body) }),
  delete: <T>(path: string) => request<T>(path, { method: 'DELETE' }),
  download: async (path: string) => (await requestBlob(path)).blob,
  downloadWithMeta: (path: string) => requestBlob(path),
}

export function formatError(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
