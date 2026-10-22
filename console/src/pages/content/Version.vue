<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => Number(route.params.id))
const v = ref<any>(null); const prev = ref<any>(null); const err = ref(''); const table = ref(''); const only = ref(true)
async function load() {
  err.value = ''
  try {
    v.value = await get(`/v1/admin/content/versions/${id.value}`)
    const list = (await get('/v1/admin/content/versions')).versions ?? []; const i = list.findIndex((x: any) => x.id === id.value); const p = list[i + 1]
    prev.value = p ? await get(`/v1/admin/content/versions/${p.id}`) : null
  } catch (e: any) { err.value = e.message }
}
onMounted(load)
const tables = computed(() => Object.keys(v.value?.bundle?.tables ?? {}).sort())
const keyOf = (r: any) => String(r.id ?? r.code ?? r.name ?? r.key ?? JSON.stringify(r))
const diff = computed(() => {
  if (!table.value) return null
  const a: any[] = v.value?.bundle?.tables?.[table.value] ?? [], b: any[] = prev.value?.bundle?.tables?.[table.value] ?? []
  const bm = new Map(b.map((r) => [keyOf(r), r])), am = new Map(a.map((r) => [keyOf(r), r]))
  const rows: { k: string; st: 'added' | 'removed' | 'changed' | 'same'; a?: any; b?: any }[] = []
  for (const [k, r] of am) { const o = bm.get(k); rows.push({ k, st: !o ? 'added' : JSON.stringify(o) === JSON.stringify(r) ? 'same' : 'changed', a: r, b: o }) }
  for (const [k, r] of bm) if (!am.has(k)) rows.push({ k, st: 'removed', b: r })
  return rows.sort((x, y) => x.k.localeCompare(y.k))
})
const summary = computed(() => { const t: Record<string, string> = {}; for (const k of tables.value) { const a: any[] = v.value.bundle.tables[k] ?? [], b: any[] = prev.value?.bundle?.tables?.[k] ?? []; const bm = new Map(b.map((r) => [keyOf(r), JSON.stringify(r)])); let add = 0, chg = 0; for (const r of a) { const o = bm.get(keyOf(r)); if (o === undefined) add++; else if (o !== JSON.stringify(r)) chg++; bm.delete(keyOf(r)) } const rem = bm.size; t[k] = add || chg || rem ? `+${add} ~${chg} −${rem}` : '' } return t })
function fields(a: any, b: any) { const ks = new Set([...Object.keys(a ?? {}), ...Object.keys(b ?? {})]); return [...ks].filter((k) => JSON.stringify(a?.[k]) !== JSON.stringify(b?.[k])) }
async function rollback() { try { await post(`/v1/admin/content/versions/${id.value}/rollback`); toast.show('rolled back — new version created'); router.push('/content') } catch (e: any) { toast.err(e) } }
const ts = (x: any) => x ? String(x).replace('T', ' ').slice(0, 19) : '—'
</script>
<template>
  <div class="crumb"><router-link to="/content">Content versions</router-link> › <span>#{{ id }}</span><span class="id" v-if="prev">diff against #{{ prev.id }}</span>
    <span style="margin-left:auto"><button class="btn s" @click="ask(`Restore the content of version #${id} as a new version?`) && rollback()">Roll back to this</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="v">
    <div class="grid"><div><Kv k="Created" :v="ts(v.created_at)" /><Kv k="Source" :v="v.source" /></div><div><Kv k="Note" :v="v.note ?? '—'" /><Kv k="Environment" :v="v.environment" /></div><div><Kv k="Sections" :v="(v.sections ?? []).join(', ')" /><Kv k="Format" :v="v.bundle?.format" /></div></div>
    <h3>Tables <small>({{ tables.length }} — click one for a row diff)</small></h3>
    <table><thead><tr><th>Table</th><th style="width:120px">Rows</th><th style="width:120px">Previous</th><th>Change vs #{{ prev?.id ?? '—' }}</th></tr></thead><tbody>
      <tr v-for="t in tables" :key="t" class="row-link" :class="{ on: table === t }" @click="table = t"><td class="m">{{ t }}</td><td>{{ (v.bundle.tables[t] ?? []).length }}</td><td class="dim">{{ (prev?.bundle?.tables?.[t] ?? []).length }}</td><td :class="summary[t] ? '' : 'dim'">{{ summary[t] || 'unchanged' }}</td></tr></tbody></table>
    <template v-if="diff">
      <h3>{{ table }} <small>({{ diff.filter((d) => d.st !== 'same').length }} changed rows)</small> <label style="margin-left:10px;font-weight:normal"><input type="checkbox" v-model="only" /> changes only</label></h3>
      <table><thead><tr><th style="width:280px">Key</th><th style="width:90px">State</th><th>Fields</th></tr></thead><tbody>
        <tr v-for="d in diff.filter((x) => !only || x.st !== 'same')" :key="d.k"><td class="m">{{ d.k }}</td><td :class="{ ok: d.st === 'added', bad: d.st === 'removed', dim: d.st === 'same' }">{{ d.st }}</td>
          <td class="m dim" style="white-space:pre-wrap">{{ d.st === 'same' ? '' : d.st === 'changed' ? fields(d.a, d.b).map((f) => `${f}: ${JSON.stringify(d.b?.[f])} → ${JSON.stringify(d.a?.[f])}`).join('\n') : JSON.stringify(d.a ?? d.b).slice(0, 400) }}</td></tr>
        <tr v-if="!diff.filter((x) => !only || x.st !== 'same').length"><td colspan="3" class="empty">No differences</td></tr></tbody></table>
    </template>
    <h3>Counts</h3><JsonView :value="v.counts" />
  </div>
</template>
