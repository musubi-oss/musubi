<script setup lang="ts">
// One world entity: where it is (cell, position, hosting simulator), who owns it, its components,
// children, and every handover it went through. Reached from the world's entity table and from
// any `entity` cross-link.
import { ref, computed, watch } from 'vue'
import { useRoute } from 'vue-router'
import { get, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { refFor } from '../../refs'
const route = useRoute()
const w = computed(() => String(route.params.w)); const id = computed(() => String(route.params.id))
const ent = ref<any>(null); const children = ref<any[]>([]); const hos = ref<any[]>([]); const err = ref(''); const busy = ref(false)
const tab = ref(String(route.query.tab ?? 'components'))
const cellKey = (c: any) => (Array.isArray(c) ? c.join(',') : c && typeof c === 'object' ? `${c.x},${c.y}` : String(c ?? '—'))
async function load() {
  busy.value = true; err.value = ''
  const base = `/v1/server/worlds/${encodeURIComponent(w.value)}/entities/${encodeURIComponent(id.value)}`
  try {
    const r = await get(base); ent.value = r.entity ?? r
    const [c, h] = await Promise.all([
      get(`${base}/children`).catch(() => ({})),
      get(`/v1/admin/worlds/${encodeURIComponent(w.value)}/handovers` + qs({ limit: 200 })).catch(() => ({})),
    ])
    children.value = c.children ?? c.entities ?? (Array.isArray(c) ? c : [])
    const all = h.handovers ?? h.steps ?? (Array.isArray(h) ? h : [])
    hos.value = all.filter((x: any) => String(x.entity ?? x.entity_id ?? x.id) === id.value)
  } catch (e: any) { err.value = e.message; ent.value = null }
  busy.value = false
}
watch([w, id], load, { immediate: true })
const comps = computed<[string, any][]>(() => Object.entries(ent.value?.components ?? {}))
const pos = computed(() => { const p = ent.value?.pos; return p ? `${p.x.toFixed(2)}, ${p.y.toFixed(2)}${p.z != null ? ', ' + p.z.toFixed(2) : ''}` : '—' })
</script>
<template>
  <div class="crumb"><router-link to="/world">Worlds</router-link> › <router-link :to="`/world/worlds/${encodeURIComponent(w)}`">{{ w }}</router-link> › <router-link :to="`/world/worlds/${encodeURIComponent(w)}?tab=entities`">entities</router-link> › <span class="m">{{ id }}</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <template v-else-if="ent">
    <div class="card">
      <div class="grid" style="grid-template-columns:repeat(4,1fr)">
        <div style="border:0;margin:0"><Kv k="cell" :v="cellKey(ent.cell)" mono /></div>
        <div style="border:0;margin:0"><Kv k="position" :v="pos" mono copy /></div>
        <div style="border:0;margin:0"><Kv k="owner (account)" :v="ent.owner ?? 'unowned — any session may drive it'" :mono="!!ent.owner" :to="refFor('owner', ent.owner, ent)" copy /></div>
        <div style="border:0;margin:0"><Kv k="simulator" :v="ent.sim ?? ent.host ?? '—'" mono /></div>
        <div style="border:0;margin:0"><Kv k="persistence" :v="ent.persistent ? 'journaled' : 'volatile'" /></div>
        <div style="border:0;margin:0"><Kv k="global" :v="ent.global ? 'yes — every subscriber' : 'no — interest-managed'" /></div>
        <div style="border:0;margin:0"><Kv k="in transit" :v="ent.remote_to ? `→ node ${ent.remote_to}` : ent.transit ? `→ ${cellKey(ent.transit.to ?? ent.transit)}` : 'no'" /></div>
        <div style="border:0;margin:0"><Kv k="parent" :v="ent.parent ?? '—'" :mono="!!ent.parent" :to="ent.parent ? `/world/worlds/${encodeURIComponent(w)}/entities/${encodeURIComponent(ent.parent)}` : null" /></div>
      </div>
    </div>
    <div class="card">
      <div class="tabs"><a :class="{ on: tab === 'components' }" @click="tab = 'components'">components ({{ comps.length }})</a><a :class="{ on: tab === 'children' }" @click="tab = 'children'">children ({{ children.length }})</a><a :class="{ on: tab === 'handovers' }" @click="tab = 'handovers'">handovers ({{ hos.length }})</a><a :class="{ on: tab === 'json' }" @click="tab = 'json'">json</a></div>
      <template v-if="tab === 'components'">
        <p class="empty" v-if="!comps.length">No components — the entity is a bare position.</p>
        <div v-for="[k, v] in comps" :key="k" style="margin-bottom:12px"><div class="k" style="margin-bottom:4px">{{ k }}</div><JsonView :value="v" /></div>
      </template>
      <DataTable v-else-if="tab === 'children'" :cols="[{ key: 'id', mono: true }, { key: 'cell', fmt: (r) => cellKey(r.cell), mono: true }, { key: 'persistent', fmt: (r) => r.persistent ? 'journaled' : 'volatile' }]" :rows="children" :loading="busy" empty="No child entities" pick @pick="(r) => $router.push(`/world/worlds/${encodeURIComponent(w)}/entities/${encodeURIComponent(r.id)}`)" />
      <DataTable v-else-if="tab === 'handovers'" :cols="[{ key: 'at', dim: true }, { key: 'from', fmt: (r) => cellKey(r.from ?? r.from_cell), mono: true }, { key: 'to', fmt: (r) => cellKey(r.to ?? r.to_cell), mono: true }, { key: 'state' }, { key: 'ms', label: 'took (ms)' }]" :rows="hos" :loading="busy" empty="No handovers recorded for this entity" />
      <JsonView v-else :value="ent" />
    </div>
  </template>
  <div class="card" v-else-if="busy"><p class="note">Loading…</p></div>
</template>
