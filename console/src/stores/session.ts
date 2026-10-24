import { defineStore } from 'pinia'
import { api, auth, get } from '../api/client'

export interface NodeInfo { name: string; roles: string[]; region: string; project?: string; environment?: string; version?: string; [k: string]: any }
export interface Whoami { key_name?: string; role?: string; project?: string; [k: string]: any }

export const useSession = defineStore('session', {
  state: () => ({ ready: false, authed: false, node: null as NodeInfo | null, me: null as Whoami | null, error: '' as string }),
  actions: {
    async probe(): Promise<boolean> {
      this.error = ''
      if (!auth.key) { this.authed = false; this.ready = true; return false }
      try {
        this.node = await get<NodeInfo>('/v1/node')
        this.authed = true
      } catch (e: any) {
        this.authed = false
        this.error = e?.status === 401 ? 'key rejected' : (e?.message ?? String(e))
      }
      this.ready = true
      return this.authed
    },
    async login(key: string, base = '') { auth.base = base; auth.key = key.trim(); return this.probe() },
    /** E-mail + password: the node issues a short-lived session key of the operator's role; we keep only that key. */
    async loginPassword(email: string, password: string, base = '') {
      this.error = ''; auth.base = base; auth.key = ''
      try {
        const r = await api<{ key: string; operator?: Whoami }>('POST', '/v1/operator/login', { email, password })
        auth.key = r.key; this.me = r.operator ?? null
      } catch (e: any) { this.error = e?.message ?? String(e); return false }
      return this.probe()
    },
    async logout() {
      try { if (auth.key) await api('DELETE', '/v1/operator/logout') } catch {}
      auth.key = ''; this.authed = false; this.me = null
    },
  },
})
