<script setup lang="ts">
// Columns: key (dot path), label, mono, fmt(row) → string. Rows clickable when `pick` listener is set.
export interface Col { key: string; label?: string; mono?: boolean; dim?: boolean; fmt?: (row: any) => string; width?: string }
const props = defineProps<{ cols: Col[]; rows: any[]; empty?: string; pick?: boolean }>()
const emit = defineEmits<{ pick: [row: any] }>()
function at(row: any, key: string) { return key.split('.').reduce((o, k) => (o == null ? o : o[k]), row) }
function show(c: Col, row: any) {
  if (c.fmt) return c.fmt(row)
  const v = at(row, c.key)
  if (v === null || v === undefined || v === '') return '—'
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (typeof v === 'object') return JSON.stringify(v)
  if (typeof v === 'number' && /(_at|At)$/.test(c.key) && v > 1e9) return new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 19) + ' UTC'
  return String(v)
}
</script>
<template>
  <table>
    <thead><tr><th v-for="c in cols" :key="c.key" :style="c.width ? { width: c.width } : undefined">{{ c.label ?? c.key }}</th><th v-if="$slots.actions"></th></tr></thead>
    <tbody>
      <tr v-for="(r, i) in rows" :key="i" :class="{ row: pick }" @click="pick && emit('pick', r)">
        <td v-for="c in cols" :key="c.key" :class="{ m: c.mono, dim: c.dim }">{{ show(c, r) }}</td>
        <td v-if="$slots.actions" @click.stop><slot name="actions" :row="r" /></td>
      </tr>
      <tr v-if="!rows.length"><td :colspan="cols.length + 1" class="empty">{{ empty ?? 'No Data' }}</td></tr>
    </tbody>
  </table>
</template>
