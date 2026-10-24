import { useQuery } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { auth } from '@/lib/auth'

export type Series = Map<string, number>

/** Prometheus text from this node's `/metrics`, as totals plus per-second rates between the last two samples. */
export function useMetrics(live: boolean, everyMs = 1000) {
  const prev = useRef<{ at: number; m: Series } | null>(null)
  const [rate, setRate] = useState<Series>(new Map())
  const q = useQuery({
    queryKey: ['metrics-text'],
    queryFn: async ({ signal }) => {
      try {
        const r = await fetch((auth.base || '') + '/metrics', { headers: { 'x-musubi-server-key': auth.key }, signal })
        return r.ok ? await r.text() : ''
      } catch { return '' }
    },
    refetchInterval: live ? everyMs : false,
    retry: false,
  })
  const [tot, setTot] = useState<Series>(new Map())
  useEffect(() => {
    if (q.data === undefined) return
    const m: Series = new Map()
    for (const line of q.data.split('\n')) { if (!line.startsWith('musubi_')) continue; const sp = line.lastIndexOf(' '); m.set(line.slice(0, sp), Number(line.slice(sp + 1))) }
    const at = performance.now(), p = prev.current
    if (p && at - p.at > 400) { const dt = (at - p.at) / 1000; const rr: Series = new Map(); for (const [k, v] of m) { const pv = p.m.get(k); if (pv != null) rr.set(k, Math.max(0, (v - pv) / dt)) } setRate(rr) }
    prev.current = { at, m }
    setTot(m)
  }, [q.dataUpdatedAt, q.data])
  return { tot, rate, ready: q.data !== undefined }
}

export function sum(src: Series, name: string, labels: Record<string, string> = {}) {
  let t = 0
  for (const [k, v] of src) {
    if (!k.startsWith(name + '{') && k !== name) continue
    if (Object.entries(labels).every(([l, val]) => k.includes(`${l}="${val}"`))) t += v
  }
  return t
}

export function series(src: Series, name: string) {
  const out: { labels: Record<string, string>; v: number }[] = []
  for (const [k, v] of src) {
    if (!k.startsWith(name + '{')) continue
    const labels: Record<string, string> = {}
    for (const m of k.slice(name.length + 1, -1).matchAll(/(\w+)="([^"]*)"/g)) labels[m[1] as string] = m[2] as string
    out.push({ labels, v })
  }
  return out
}

export const fmtN = (v?: number | null) => (v == null ? '—' : v >= 1e6 ? (v / 1e6).toFixed(2) + ' M' : v >= 1e4 ? (v / 1e3).toFixed(1) + ' k' : v >= 100 || Number.isInteger(v) ? v.toFixed(0) : v.toFixed(1))
export const mb = (b?: number | null) => (b == null ? '—' : b >= 1048576 ? (b / 1048576).toFixed(1) + ' MB' : (b / 1024).toFixed(0) + ' KB')
