// Thin client over the admin/server API. The operator key travels as x-musubi-server-key;
// it lives in localStorage and is never embedded in URLs except the SSO return hash.
export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string, public body: any) { super(message) }
}
import { reactive } from 'vue'
// Number of requests in flight; the layout shows a progress bar while it is > 0.
export const net = reactive({ busy: 0 })
const KEY = 'musubi.console.key'
const BASE = 'musubi.console.base'
export const auth = {
  get key(): string { return localStorage.getItem(KEY) ?? '' },
  set key(v: string) { v ? localStorage.setItem(KEY, v) : localStorage.removeItem(KEY) },
  get base(): string { return localStorage.getItem(BASE) ?? '' },
  set base(v: string) { v ? localStorage.setItem(BASE, v.replace(/\/+$/, '')) : localStorage.removeItem(BASE) },
}
export async function api<T = any>(method: string, path: string, body?: unknown, opts: { raw?: boolean; headers?: Record<string, string> } = {}): Promise<T> {
  const h: Record<string, string> = { 'x-musubi-server-key': auth.key, ...(opts.headers ?? {}) }
  let payload: BodyInit | undefined
  if (body instanceof Uint8Array || body instanceof Blob) { payload = body as BodyInit; h['content-type'] ||= 'application/octet-stream' }
  else if (body !== undefined) { payload = JSON.stringify(body); h['content-type'] = 'application/json' }
  net.busy++
  let r: Response
  try { r = await fetch(auth.base + path, { method, headers: h, body: payload }) } finally { net.busy-- }
  if (opts.raw) return r as unknown as T
  const text = await r.text()
  let v: any = null
  const json = (r.headers.get('content-type') ?? '').includes('json')
  try { v = text ? (json ? JSON.parse(text) : text) : null } catch { v = text }
  if (!r.ok) {
    const msg = (v && typeof v === 'object' && (v.message || v.error)) || r.statusText || `HTTP ${r.status}`
    throw new ApiError(r.status, (v && v.code) || String(r.status), msg, v)
  }
  return v as T
}
export const get = <T = any>(p: string) => api<T>('GET', p)
export const post = <T = any>(p: string, b?: unknown) => api<T>('POST', p, b)
export const put = <T = any>(p: string, b?: unknown) => api<T>('PUT', p, b)
export const patch = <T = any>(p: string, b?: unknown) => api<T>('PATCH', p, b)
export const del = <T = any>(p: string, b?: unknown) => api<T>('DELETE', p, b)
export const qs = (o: Record<string, unknown>) => {
  const u = new URLSearchParams()
  for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== '') u.set(k, String(v))
  const s = u.toString(); return s ? `?${s}` : ''
}
