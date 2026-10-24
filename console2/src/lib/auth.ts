// Operator session: the key travels as x-musubi-server-key and lives in localStorage; the node
// issues it (e-mail + password / SSO) or the operator pastes a server key.
import { useSyncExternalStore } from 'react'

const KEY = 'musubi.console.key'
const BASE = 'musubi.console.base'
const MODE = 'musubi.console.login'
export type LoginMode = 'password' | 'key'

const listeners = new Set<() => void>()
function emit() { for (const l of listeners) l() }

export const auth = {
  get key(): string { return localStorage.getItem(KEY) ?? '' },
  set key(v: string) { v ? localStorage.setItem(KEY, v) : localStorage.removeItem(KEY); emit() },
  get base(): string { return localStorage.getItem(BASE) ?? '' },
  set base(v: string) { v ? localStorage.setItem(BASE, v.replace(/\/+$/, '')) : localStorage.removeItem(BASE); emit() },
  get mode(): LoginMode { return localStorage.getItem(MODE) === 'key' ? 'key' : 'password' },
  set mode(v: LoginMode) { localStorage.setItem(MODE, v) },
  get authed(): boolean { return !!this.key },
  clear() { this.key = '' },
}

export function useAuth() {
  return useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l) },
    () => auth.key,
  )
}
