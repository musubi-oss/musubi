export function ago(v: unknown): string {
  if (!v) return '—'
  const d = typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : +new Date(String(v))
  const sec = Math.round((Date.now() - d) / 1000)
  if (!Number.isFinite(sec)) return '—'
  return sec < 60 ? `${sec}s ago` : sec < 3600 ? `${Math.round(sec / 60)} min ago` : sec < 86400 ? `${Math.round(sec / 3600)} h ago` : `${Math.round(sec / 86400)} d ago`
}
export const short = (id: unknown) => (id ? String(id).slice(0, 8) : '—')
export const pct = (v?: number | null) => (v == null ? '—' : (v * 100).toFixed(0) + '%')
export function uptime(u?: number | null): string {
  if (u == null) return '—'
  return u < 3600 ? `${Math.round(u / 60)} min` : u < 86400 ? `${(u / 3600).toFixed(1)} h` : `${(u / 86400).toFixed(1)} d`
}

/** Timestamp (ISO string or unix secs/ms) → `YYYY-MM-DD HH:MM:SS`, `—` when empty. */
export function ts(v: unknown): string {
  if (v === null || v === undefined || v === '') return '—'
  const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(String(v))
  return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19)
}
