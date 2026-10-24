// Typed client over the node's OpenAPI document (src/api/schema.d.ts is generated from
// openapi.json): `api.GET('/v1/admin/accounts')` fails to compile for a path the node does not
// serve. `raw()` is the untyped escape hatch for catalog-driven pages whose paths are data.
import createClient, { type Middleware } from 'openapi-fetch'
import type { paths } from '@/api/schema'
import { auth } from './auth'

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string, public body: unknown) { super(message) }
}

function messageOf(status: number, body: unknown, statusText: string): string {
  if (body && typeof body === 'object') {
    const b = body as Record<string, unknown>
    if (typeof b.message === 'string') return b.message
    if (typeof b.error === 'string') return b.error
  }
  return statusText || `HTTP ${status}`
}

const authMiddleware: Middleware = {
  onRequest({ request }) {
    if (auth.key) request.headers.set('x-musubi-server-key', auth.key)
    return request
  },
  async onResponse({ response, request }) {
    if (response.status === 401 && auth.key && !request.url.includes('/v1/operator/login')) {
      auth.clear()
      if (!location.pathname.endsWith('/login')) location.assign('/console/login?to=' + encodeURIComponent(location.pathname.replace(/^\/console/, '') + location.search))
    }
    return response
  },
}

export const api = createClient<paths>({ baseUrl: auth.base || '/' })
api.use(authMiddleware)

/** Any `/v1/...` path (templates from the catalog); throws ApiError on non-2xx. */
export async function raw<T = unknown>(method: string, path: string, body?: unknown, init: { headers?: Record<string, string>; signal?: AbortSignal } = {}): Promise<T> {
  const h: Record<string, string> = { ...(init.headers ?? {}) }
  if (auth.key) h['x-musubi-server-key'] = auth.key
  let payload: BodyInit | undefined
  if (body instanceof Uint8Array || body instanceof Blob) { payload = body as BodyInit; h['content-type'] ||= 'application/octet-stream' }
  else if (body !== undefined) { payload = JSON.stringify(body); h['content-type'] = 'application/json' }
  const r = await fetch(auth.base + path, { method, headers: h, body: payload, signal: init.signal })
  const text = await r.text()
  const isJson = (r.headers.get('content-type') ?? '').includes('json')
  let v: unknown = null
  try { v = text ? (isJson ? JSON.parse(text) : text) : null } catch { v = text }
  if (r.status === 401 && auth.key && !path.startsWith('/v1/operator/login')) {
    auth.clear()
    location.assign('/console/login')
  }
  if (!r.ok) {
    const code = v && typeof v === 'object' && typeof (v as { code?: unknown }).code === 'string' ? (v as { code: string }).code : String(r.status)
    throw new ApiError(r.status, code, messageOf(r.status, v, r.statusText), v)
  }
  return v as T
}

export const qs = (o: Record<string, unknown>) => {
  const u = new URLSearchParams()
  for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== '') u.set(k, String(v))
  const s = u.toString()
  return s ? `?${s}` : ''
}
