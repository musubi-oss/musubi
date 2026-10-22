<script setup lang="ts">
// Columns: key (dot path), label, mono, dim, fmt(row) → string, width, sort=false to pin a column.
// Rows are sorted/paged on the client (most list endpoints return the whole set); `pick` makes rows clickable,
// mono cells copy their value on click of the small copy glyph. Page size is remembered per browser.
import { ref, computed, watch } from 'vue'
import { toast } from '../stores/toast'
export interface Col { key: string; label?: string; mono?: boolean; dim?: boolean; fmt?: (row: any) => string; width?: string; sort?: boolean; num?: boolean }
const props = withDefaults(defineProps<{ cols: Col[]; rows: any[]; empty?: string; pick?: boolean; loading?: boolean; pageSize?: number; id?: string }>(), { pageSize: 0 })
const emit = defineEmits<{ pick: [row: any] }>()
const SIZES = [25, 50, 100, 500]
const stored = Number(localStorage.getItem('dt.size') || 0)
const size = ref(props.pageSize || stored || 50)
const page = ref(0)
// Newest first by default when the table has a creation/time column; a header click overrides.
const defaultKey = () => props.cols.find((c) => c.sort !== false && /^(created_at|created|at|ts|time|started_at|received_at|updated_at)$/.test(c.key))?.key ?? ''
const sortKey = ref(defaultKey()); const sortDir = ref<1 | -1>(-1)
watch(() => props.cols.map((c) => c.key).join(), () => { if (!props.cols.some((c) => c.key === sortKey.value)) { sortKey.value = defaultKey(); sortDir.value = -1 } })
function at(row: any, key: string) { return key.split('.').reduce((o, k) => (o == null ? o : o[k]), row) }
const ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d/
function tsMs(c: Col, v: unknown): number | null {
  if (typeof v === 'number' && /(_at|At|_until|_ts|_since)$/.test(c.key) && v > 1e9) return v * (v < 1e12 ? 1000 : 1)
  if (typeof v === 'string' && ISO.test(v)) { const n = Date.parse(v); return Number.isNaN(n) ? null : n }
  return null
}
function fmtTs(ms: number) { return new Date(ms).toISOString().replace('T', ' ').slice(0, 19) + ' UTC' }
function ago(ms: number) {
  const d = Date.now() - ms; const a = Math.abs(d); const s = d < 0 ? 'in ' : ''; const e = d < 0 ? '' : ' ago'
  if (a < 60e3) return 'just now'; if (a < 3600e3) return `${s}${Math.round(a / 60e3)} min${e}`; if (a < 86400e3) return `${s}${Math.round(a / 3600e3)} h${e}`
  if (a < 30 * 86400e3) return `${s}${Math.round(a / 86400e3)} d${e}`; return fmtTs(ms).slice(0, 10)
}
function show(c: Col, row: any) {
  if (c.fmt) return c.fmt(row)
  const v = at(row, c.key)
  if (v === null || v === undefined || v === '') return '—'
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (Array.isArray(v)) return v.length ? (v.every((x) => typeof x !== 'object') ? v.join(', ') : `${v.length} items`) : '—'
  if (typeof v === 'object') { const s = JSON.stringify(v); return s.length > 80 ? s.slice(0, 77) + '…' : s }
  const ms = tsMs(c, v); if (ms !== null) return ago(ms)
  if (typeof v === 'number') return Number.isInteger(v) ? v.toLocaleString('en-US') : String(v)
  return String(v)
}
function title(c: Col, row: any): string | undefined {
  const v = at(row, c.key)
  const ms = tsMs(c, v); if (ms !== null) return fmtTs(ms)
  if (v && typeof v === 'object') { const s = JSON.stringify(v, null, 1); return s.length > 2000 ? s.slice(0, 2000) + '…' : s }
  if (typeof v === 'string' && v.length > 40) return v
  return undefined
}
function copyable(c: Col, row: any) { const v = at(row, c.key); return c.mono && v !== null && v !== undefined && v !== '' && typeof v !== 'object' }
function copy(c: Col, row: any) { navigator.clipboard?.writeText(String(at(row, c.key))); toast.show('copied') }
function sortBy(c: Col) {
  if (c.sort === false) return
  if (sortKey.value !== c.key) { sortKey.value = c.key; sortDir.value = 1 }
  else if (sortDir.value === 1) sortDir.value = -1
  else sortKey.value = ''
  page.value = 0
}
const sorted = computed(() => {
  if (!sortKey.value) return props.rows
  const c = props.cols.find((x) => x.key === sortKey.value)
  const val = (r: any) => { if (c?.fmt && !c.num) return c.fmt(r); const v = at(r, sortKey.value); const ms = c ? tsMs(c, v) : null; return ms ?? v }
  const dir = sortDir.value
  return [...props.rows].sort((a, b) => {
    const x = val(a), y = val(b)
    if (x == null || x === '') return y == null || y === '' ? 0 : 1
    if (y == null || y === '') return -1
    if (typeof x === 'number' && typeof y === 'number') return (x - y) * dir
    if (typeof x === 'boolean' && typeof y === 'boolean') return (Number(x) - Number(y)) * dir
    return String(x).localeCompare(String(y), undefined, { numeric: true, sensitivity: 'base' }) * dir
  })
})
const pages = computed(() => Math.max(1, Math.ceil(sorted.value.length / size.value)))
const shown = computed(() => sorted.value.slice(page.value * size.value, (page.value + 1) * size.value))
const from = computed(() => (sorted.value.length ? page.value * size.value + 1 : 0))
const to = computed(() => Math.min(sorted.value.length, (page.value + 1) * size.value))
watch(() => props.rows.length, () => { if (page.value >= pages.value) page.value = Math.max(0, pages.value - 1) })
function setSize(e: Event) { size.value = Number((e.target as HTMLSelectElement).value); page.value = 0; if (!props.pageSize) localStorage.setItem('dt.size', String(size.value)) }
</script>
<template>
  <table :class="{ loading }">
    <thead><tr>
      <th v-for="c in cols" :key="c.key" :style="c.width ? { width: c.width } : undefined" :class="{ sortable: c.sort !== false, on: sortKey === c.key }" @click="sortBy(c)">
        {{ c.label ?? c.key }}<span class="arrow" v-if="sortKey === c.key">{{ sortDir === 1 ? '▲' : '▼' }}</span>
      </th><th v-if="$slots.actions"></th></tr></thead>
    <tbody>
      <tr v-for="(r, i) in shown" :key="i" :class="{ row: pick }" @click="pick && emit('pick', r)">
        <td v-for="c in cols" :key="c.key" :class="{ m: c.mono, dim: c.dim, cpc: copyable(c, r) }" :title="title(c, r)">
          <span class="cell">{{ show(c, r) }}</span><button v-if="copyable(c, r)" class="cp" title="copy" @click.stop="copy(c, r)"></button>
        </td>
        <td v-if="$slots.actions" @click.stop><slot name="actions" :row="r" /></td>
      </tr>
      <tr v-if="!rows.length"><td :colspan="cols.length + 1" class="empty">{{ loading ? 'Loading…' : empty ?? 'No Data' }}</td></tr>
    </tbody>
  </table>
  <div class="pager" v-if="pages > 1 || size !== SIZES[1]">
    <span>{{ from.toLocaleString('en-US') }}–{{ to.toLocaleString('en-US') }} of {{ sorted.length.toLocaleString('en-US') }}</span>
    <select class="sel sz" :value="size" @change="setSize"><option v-for="s in SIZES" :key="s" :value="s">{{ s }} / page</option></select>
    <button class="btn s" :disabled="page === 0" @click="page = 0" title="first">«</button>
    <button class="btn s" :disabled="page === 0" @click="page--">Prev</button>
    <span class="pg">{{ page + 1 }} / {{ pages }}</span>
    <button class="btn s" :disabled="page >= pages - 1" @click="page++">Next</button>
    <button class="btn s" :disabled="page >= pages - 1" @click="page = pages - 1" title="last">»</button>
  </div>
</template>
