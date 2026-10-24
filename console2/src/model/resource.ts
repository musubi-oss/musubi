// Framework-free helpers shared by the catalog-driven list and detail pages.
import type { Res } from './catalog'

export type Row = Record<string, any>

/** `{field}` placeholders from the row; `{id}` falls back to the record key. */
export function fill(path: string, row: Row | null | undefined, id?: string): string {
  return path.replace(/\{([^}]+)\}/g, (_, k: string) => encodeURIComponent(String(k === 'id' && id !== undefined ? id : row?.[k] ?? id ?? '')))
}

/** The list payload is either an array, `{<rowsKey>: [...]}` or the first array-valued field. */
export function pickRows(d: unknown, rowsKey?: string, key = 'id'): Row[] {
  let list: unknown = d
  if (!Array.isArray(d)) {
    if (!d || typeof d !== 'object') return []
    const o = d as Row
    list = rowsKey ? o[rowsKey] : Object.values(o).find(Array.isArray)
  }
  if (!Array.isArray(list)) return []
  // a list of names/ids (worlds, queues) becomes one-column rows keyed by the resource key
  return list.map((r) => (r !== null && typeof r === 'object' ? (r as Row) : { [key]: r }))
}

export const rowKey = (res: Res, row: Row): string | undefined => {
  const k = row[res.key ?? 'id'] ?? row.id ?? row.name
  return k === undefined || k === null ? undefined : String(k)
}

export function itemPath(res: Res, row: Row): string | null {
  if (!res.item) return null
  return res.item.replace(/\{([^}]+)\}/g, (_, k: string) => encodeURIComponent(String(row[k] ?? row[res.key ?? 'id'] ?? row.id ?? row.name ?? '')))
}

export function detailPath(section: string, res: Res, row: Row): string | null {
  if (res.link) return res.link(row)
  if (res.noDetail) return null
  const k = rowKey(res, row)
  return k === undefined ? null : `/${section}/${res.id}/${encodeURIComponent(k)}`
}

export const isMonoKey = (k: string) => /(^|_)(id|sha256|hash|address|code)$/.test(k)

export function fmtVal(k: string, v: unknown): string {
  if (v === null || v === undefined || v === '') return '—'
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (typeof v === 'number' && /(_at|At|_until|_ts|_since)$/.test(k) && v > 1e9) return new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 19) + ' UTC'
  if (typeof v === 'object') return JSON.stringify(v)
  return String(v)
}

/** "feature is off in this node's config" responses, which are not errors for the operator. */
export function featureOff(e: { status?: number; message?: string } | null | undefined): string | null {
  if (!e) return null
  if ((e.status === 400 || e.status === 404) && /not (enabled|running|configured|available)/i.test(e.message ?? '')) return (e.message ?? '').replace(/^(invalid argument|not found): /, '')
  return null
}

export const toCsv = (cols: string[], rows: Row[]) =>
  [cols.join(','), ...rows.map((r) => cols.map((c) => { const v = r[c]; const s = v === null || v === undefined ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v); return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s }).join(','))].join('\n')
