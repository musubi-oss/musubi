<script setup lang="ts">
// Authoritative matches across the cluster (snapshots every node publishes), so an operator
// sees where a match runs and whether its snapshot is still fresh.
import { ref, computed, onMounted } from 'vue'
import { get } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
const x = ref<Record<string, any>>({}); const q = ref(''); const module = ref(''); const err = ref('')
async function load() { err.value = ''; try { const [m, n] = await Promise.all([get('/v1/admin/cluster/matches'), get('/v1/server/cluster/nodes').catch(() => null)]); x.value.m = m; x.value.nodes = n } catch (e: any) { err.value = e.message } }
onMounted(load)
const name = (id?: string) => (x.value.nodes?.nodes ?? []).find((n: any) => n.id === id)?.name ?? (id ? '…' + id.slice(-8) : '—')
// a match whose node is dead or no longer registered is waiting for adoption
const gone = (id?: string) => { if (!x.value.nodes) return false; const n = (x.value.nodes.nodes ?? []).find((n: any) => n.id === id); return !n || !n.alive }
const ago = (v: number) => { const s = Math.round(Date.now() / 1000 - v); return s < 60 ? `${s}s ago` : s < 3600 ? `${Math.round(s / 60)}m ago` : s < 86400 ? `${(s / 3600).toFixed(1)}h ago` : `${Math.round(s / 86400)}d ago` }
const rows = computed(() => (x.value.m?.matches ?? []).filter((m: any) => (!module.value || m.module === module.value) && (!q.value || m.match_id.includes(q.value) || (m.label ?? '').includes(q.value))).map((m: any) => ({ ...m, node: name(m.node_id), stale: Date.now() / 1000 - m.updated_at > 60, orphan: gone(m.node_id) })))
const modules = computed(() => [...new Set<string>((x.value.m?.matches ?? []).map((m: any) => m.module))].sort())
</script>
<template>
  <div class="stats">
    <div class="stat"><div class="k">Matches</div><div class="v">{{ (x.m?.matches ?? []).length }}<small>{{ x.m?.local ?? 0 }} on this node</small></div></div>
    <div class="stat"><div class="k">Modules</div><div class="v">{{ modules.length }}</div></div>
    <div class="stat"><div class="k">On dead or gone nodes</div><div class="v" :class="{ warn: rows.some((r: any) => r.orphan) }">{{ rows.filter((r: any) => r.orphan).length }}<small>adopted on next sweep</small></div></div>
  </div>
  <div class="card">
    <h2>Matches <small>({{ rows.length }})</small></h2>
    <p class="err" v-if="err">{{ err }}</p>
    <div class="tools"><input class="in" v-model="q" placeholder="match id or label" /><select class="in" v-model="module"><option value="">all modules</option><option v-for="m in modules" :key="m" :value="m">{{ m }}</option></select><button class="btn" @click="load">Refresh</button></div>
    <DataTable :cols="[{ key: 'match_id', mono: true }, { key: 'module' }, { key: 'label', fmt: (r) => r.label || '—' }, { key: 'node', fmt: (r) => r.node + (r.orphan ? ' (gone)' : '') }, { key: 'tick' }, { key: 'updated_at', label: 'snapshot', fmt: (r) => ago(r.updated_at), dim: true }]" :rows="rows" pick @pick="(r) => (x.pick = r)" empty="No authoritative matches are running" />
    <p class="note" style="margin-top:10px">Each row is the latest snapshot a node published for a match; a match whose node is dead is picked up by another node from that snapshot.</p>
  </div>
  <Drawer v-if="x.pick" title="Match snapshot" @close="x.pick = null"><JsonView :value="x.pick" /></Drawer>
</template>
