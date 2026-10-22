// Client-side sort + paging for hand-written tables: `const pg = usePager(() => rows.value)` then
// `<tr v-for="r in pg.rows">` and `<Pager :p="pg" />`; `<th @click="pg.sort('key')">` toggles sorting.
import { reactive, computed, watch, unref, type ComputedRef, type Ref } from 'vue'
const SIZES = [25, 50, 100, 500]
const ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d/
function norm(v: any): any {
  if (v === null || v === undefined || v === '') return null
  if (typeof v === 'string' && ISO.test(v)) { const n = Date.parse(v); return Number.isNaN(n) ? v : n }
  return v
}
export function usePager<T = any>(source: () => T[] | Ref<T[]> | ComputedRef<T[]> | null | undefined, opts: { size?: number; key?: string; dir?: 1 | -1 } = {}) {
  const stored = Number(localStorage.getItem('dt.size') || 0)
  const st = reactive({ page: 0, size: opts.size || stored || 50, key: opts.key ?? '', dir: (opts.dir ?? 1) as 1 | -1 })
  const at = (r: any, k: string) => k.split('.').reduce((o, x) => (o == null ? o : o[x]), r)
  const sorted: ComputedRef<T[]> = computed(() => {
    const rows = (unref(source()) ?? []) as T[]
    if (!st.key) return rows
    const d = st.dir
    return [...rows].sort((a: any, b: any) => {
      const x = norm(at(a, st.key)), y = norm(at(b, st.key))
      if (x === null) return y === null ? 0 : 1
      if (y === null) return -1
      if (typeof x === 'number' && typeof y === 'number') return (x - y) * d
      if (typeof x === 'boolean' && typeof y === 'boolean') return (Number(x) - Number(y)) * d
      return String(x).localeCompare(String(y), undefined, { numeric: true, sensitivity: 'base' }) * d
    })
  })
  const pages = computed(() => Math.max(1, Math.ceil(sorted.value.length / st.size)))
  const rows = computed(() => sorted.value.slice(st.page * st.size, (st.page + 1) * st.size))
  watch(() => sorted.value.length, () => { if (st.page >= pages.value) st.page = Math.max(0, pages.value - 1) })
  const pg = reactive({
    st, SIZES,
    get rows() { return rows.value }, get total() { return sorted.value.length }, get pages() { return pages.value },
    get from() { return sorted.value.length ? st.page * st.size + 1 : 0 }, get to() { return Math.min(sorted.value.length, (st.page + 1) * st.size) },
    get shown() { return pages.value > 1 || st.size !== 50 },
    sort(k: string) { if (st.key !== k) { st.key = k; st.dir = 1 } else if (st.dir === 1) st.dir = -1; else st.key = ''; st.page = 0 },
    arrow(k: string) { return st.key === k ? (st.dir === 1 ? ' ▲' : ' ▼') : '' },
    setSize(n: number) { st.size = n; st.page = 0; if (!opts.size) localStorage.setItem('dt.size', String(n)) },
    first() { st.page = 0 }, prev() { st.page = Math.max(0, st.page - 1) }, next() { st.page = Math.min(pages.value - 1, st.page + 1) }, last() { st.page = pages.value - 1 }
  })
  return pg
}
export type Pager = ReturnType<typeof usePager>
