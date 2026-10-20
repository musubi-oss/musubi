<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const w = computed(() => String(route.params.w))
const S = computed(() => `/v1/server/worlds/${w.value}`), A = computed(() => `/v1/admin/worlds/${w.value}`)
const world = ref<any>(null); const err = ref(''); const x = ref<Record<string, any>>({})
const tab = ref(String(route.query.tab ?? 'overview'))
const tabs: [string, string][] = [['overview', 'overview'], ['cells', 'cells'], ['sims', 'simulators & leases'], ['entities', 'entities'], ['journal', 'journal'], ['at', 'time travel'], ['view', 'cell view'], ['config', 'config']]
const dlg = ref<'' | 'host' | 'rewind' | 'spawn'>(''); const form = ref<any>({})
const tick = ref(0); const cx = ref(0); const cy = ref(0); const entQ = ref(''); const jFrom = ref<number | null>(null)
async function load() { err.value = ''; try { world.value = await get(S.value) } catch (e: any) { err.value = e.message } }
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = e.status === 404 ? null : { error: e.message } } }
function loadTab(t: string) {
  if (t === 'entities' || t === 'sims') fx('entities', `${A.value}/entities` + qs({ q: entQ.value || undefined, limit: 500 }))
  if (t === 'journal') fx('history', `${A.value}/history` + qs({ from_tick: jFrom.value ?? undefined, limit: 200 }))
  if (t === 'at') fx('at', `${A.value}/at` + qs({ tick: tick.value }))
  if (t === 'view') fx('view', `${S.value}/view/${cx.value}/${cy.value}`)
}
watch(tab, (t) => { router.replace({ query: { ...route.query, tab: t } }); loadTab(t) }, { immediate: true })
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok + (r && typeof r === 'object' && 'ok' in r ? '' : '')); dlg.value = ''; await load(); loadTab(tab.value) } catch (e: any) { toast.err(e) } }
const ask = (m: string) => window.confirm(m)
const list = (k: string, sub?: string) => { const v = x.value[k]; if (!v || v.error) return []; const a = sub ? v[sub] : Array.isArray(v) ? v : Object.values(v).find(Array.isArray); return Array.isArray(a) ? a : [] }
const cells = computed(() => (world.value?.cells ?? []).map((c: any) => typeof c === 'string' ? { id: c } : c))
function cellKey(c: any) { return c.id ?? c.key ?? (c.x != null ? `${c.x},${c.y}${c.z != null ? ',' + c.z : ''}${c.depth != null ? ' d' + c.depth : ''}` : JSON.stringify(c)) }
function openDlg(k: typeof dlg.value) { dlg.value = k; form.value = k === 'host' ? { x: 0, y: 0 } : k === 'rewind' ? { tick: Math.max(0, (world.value?.tick ?? 0) - 100) } : { kind: 'npc', x: 0, y: 0, persistent: true, data: '{}' } }
</script>
<template>
  <div class="crumb"><router-link to="/world">Worlds</router-link> › <span class="m">{{ w }}</span>
    <span style="margin-left:auto;display:flex;gap:8px">
      <button class="btn s" @click="act(() => post(`${A}/snapshot`), 'snapshot written')">Snapshot</button>
      <button class="btn s" @click="act(async () => { const r = await post(`${A}/verify`); toast.show(r.ok === false ? 'verify: problems found' : 'verify: ok'); x.verify = r }, 'verified')">Verify</button>
      <button class="btn s" @click="openDlg('host')">Host cell here</button>
      <button class="btn s" @click="openDlg('spawn')">Spawn entity</button>
      <button class="btn s" @click="openDlg('rewind')">Rewind</button>
      <button class="btn s d" @click="ask('Detach every simulator from this world?') && act(() => api('DELETE', `${S}/sims`), 'simulators detached')">Detach sims</button>
      <button class="btn s d" @click="ask(`Delete world ${w}?`) && act(async () => { await api('DELETE', S); router.push('/world') }, 'deleted')">Delete</button>
    </span>
  </div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="world">
    <div class="tabs"><a v-for="[t, l] in tabs" :key="t" :class="{ on: tab === t }" @click="tab = t">{{ l }}</a></div>

    <template v-if="tab === 'overview'">
      <div class="stats" style="margin-bottom:18px">
        <div class="stat"><div class="k">Tick</div><div class="v">{{ world.tick }}</div></div>
        <div class="stat"><div class="k">Entities</div><div class="v">{{ world.entities }}</div></div>
        <div class="stat"><div class="k">Cells hot / cold</div><div class="v">{{ cells.length }}<small>/ {{ (world.cold_cells ?? []).length }} cold</small></div></div>
        <div class="stat"><div class="k">Simulators</div><div class="v">{{ (world.sims ?? []).length }}<small>{{ (world.orphaned ?? []).length }} orphaned cells</small></div></div>
        <div class="stat"><div class="k">Subscribers</div><div class="v">{{ world.subscribers }}<small>{{ (world.replicas ?? []).length }} replicas · {{ (world.mirrors ?? []).length }} mirrors</small></div></div>
      </div>
      <div class="grid">
        <div><Kv k="Cell size" :v="world.config?.cell_size" /><Kv k="Partition" :v="world.config?.three_d ? 'octree' : world.config?.max_depth > 0 ? `quadtree, depth ≤ ${world.config.max_depth}` : 'fixed grid'" /><Kv k="Ghost margin / hysteresis" :v="`${world.config?.ghost_margin} / ${world.config?.hysteresis}`" /></div>
        <div><Kv k="Lease TTL / handover timeout" :v="`${world.config?.lease_ttl_ticks} / ${world.config?.handover_timeout_ticks} ticks`" /><Kv k="Hibernate after" :v="world.config?.hibernate_after_ticks ? world.config.hibernate_after_ticks + ' ticks' : 'never'" /><Kv k="History kept" :v="world.config?.history_ticks + ' ticks'" /></div>
        <div><Kv k="Client budget" :v="`${world.config?.client_budget_bytes} B, min ${world.config?.client_min_delta_ticks} ticks`" /><Kv k="Keyframe every" :v="world.config?.keyframe_ticks + ' ticks'" /><Kv k="Position quantum" :v="world.config?.pos_quantum" /></div>
      </div>
      <h3 v-if="x.verify">Last verify</h3><JsonView v-if="x.verify" :value="x.verify" />
      <h3>Orphaned cells <small>({{ (world.orphaned ?? []).length }})</small></h3>
      <div class="dim" v-if="!(world.orphaned ?? []).length">None — every hot cell has a simulator</div>
      <div v-else><span v-for="c in world.orphaned" :key="cellKey(c)" class="pill r" style="margin:0 6px 6px 0">{{ cellKey(c) }}</span></div>
    </template>

    <template v-else-if="tab === 'cells'">
      <h3 style="margin-top:0">Hot cells <small>({{ cells.length }})</small></h3>
      <DataTable :cols="[{ key: 'id', label: 'cell', fmt: cellKey, mono: true }, { key: 'owner', label: 'simulator', mono: true, fmt: (r) => r.owner ?? r.sim ?? r.node ?? '—' }, { key: 'entities' }, { key: 'players' }, { key: 'lease_until', label: 'lease until (tick)' }, { key: 'state', fmt: (r) => r.state ?? (r.hibernated ? 'hibernated' : 'live') }]" :rows="cells" pick @pick="(r) => { if (r.x != null) { cx = r.x; cy = r.y; tab = 'view' } }" />
      <h3>Cold cells <small>({{ (world.cold_cells ?? []).length }})</small></h3>
      <div class="dim" v-if="!(world.cold_cells ?? []).length">None</div>
      <div v-else><span v-for="c in world.cold_cells" :key="cellKey(c)" class="pill" style="margin:0 6px 6px 0">{{ cellKey(c) }}</span></div>
      <h3>Replicas & mirrors</h3><JsonView :value="{ replicas: world.replicas ?? [], mirrors: world.mirrors ?? [] }" />
    </template>

    <template v-else-if="tab === 'sims'">
      <h3 style="margin-top:0">Simulators <small>({{ (world.sims ?? []).length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true, fmt: (r) => typeof r === 'string' ? r : r.id ?? r.sim_id ?? '—' }, { key: 'node', mono: true, fmt: (r) => r.node ?? r.node_id ?? '—' }, { key: 'cells', fmt: (r) => Array.isArray(r.cells) ? String(r.cells.length) : r.cells ?? '—' }, { key: 'load' }, { key: 'last_seen_tick', label: 'last seen (tick)' }]" :rows="(world.sims ?? []).map((s: any) => typeof s === 'string' ? { id: s } : s)" />
      <h3>Leases <small>({{ list('entities', 'leases').length }})</small></h3>
      <DataTable :cols="[{ key: 'cell', fmt: (r) => cellKey(r.cell ?? r), mono: true }, { key: 'sim', mono: true, fmt: (r) => r.sim ?? r.owner ?? '—' }, { key: 'until', label: 'until (tick)', fmt: (r) => r.until ?? r.expires_tick ?? '—' }, { key: 'fenced' }]" :rows="list('entities', 'leases')" />
      <div class="note" style="margin-top:10px">Home node: {{ x.entities?.home_node ?? 'pending adoption' }} · this shard from node {{ x.entities?.node_id }}</div>
    </template>

    <template v-else-if="tab === 'entities'">
      <div class="tools"><input class="in" v-model="entQ" placeholder="filter id / kind" @keyup.enter="loadTab('entities')" /><button class="btn" @click="loadTab('entities')">Refresh</button><span class="dim">tick {{ x.entities?.tick }} · this node's shard</span></div>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'cell', fmt: (r) => r.cell ? cellKey(r.cell) : '—', mono: true }, { key: 'pos', fmt: (r) => r.pos ? `${r.pos.x}, ${r.pos.y}${r.pos.z != null ? ', ' + r.pos.z : ''}` : r.x != null ? `${r.x}, ${r.y}` : '—', mono: true }, { key: 'owner', mono: true }, { key: 'persistent' }, { key: 'transit', fmt: (r) => r.transit ?? r.state ?? '—' }, { key: 'version' }]" :rows="list('entities', 'entities')" pick @pick="(r) => { x.ent = r }" />
      <Drawer v-if="x.ent" title="Entity" @close="x.ent = null"><JsonView :value="x.ent" />
        <div class="form"><div class="row"><button class="btn d" @click="ask('Despawn this entity?') && act(() => post(`${S}/despawn`, { id: x.ent.id }), 'despawned').then(() => (x.ent = null))">Despawn</button></div></div></Drawer>
    </template>

    <template v-else-if="tab === 'journal'">
      <div class="tools"><input class="in" type="number" v-model.number="jFrom" placeholder="from tick" /><button class="btn" @click="loadTab('journal')">Load</button><span class="dim" v-if="x.history">{{ x.history.total }} ops · ticks {{ x.history.from_tick }} → {{ x.history.to_tick === 18446744073709551615 ? 'now' : x.history.to_tick }}</span></div>
      <table><thead><tr><th style="width:90px">Tick</th><th style="width:140px">Op</th><th style="width:220px">Entity</th><th>Payload</th></tr></thead><tbody>
        <tr v-for="(o, i) in list('history', 'ops')" :key="i"><td class="dim">{{ o.tick }}</td><td>{{ o.op ?? o.kind }}</td><td class="m">{{ o.id ?? o.entity ?? '—' }}</td><td class="m dim" style="word-break:break-all">{{ JSON.stringify(o.data ?? o.payload ?? o) }}</td></tr>
        <tr v-if="!list('history', 'ops').length"><td colspan="4" class="empty">No journal ops in range</td></tr></tbody></table>
    </template>

    <template v-else-if="tab === 'at'">
      <div class="tools"><input class="in" type="number" v-model.number="tick" placeholder="tick" /><button class="btn p" @click="loadTab('at')">Reconstruct</button><span class="dim" v-if="x.at">snapshots {{ (x.at.from_snapshots ?? []).map((s: any) => `${s.tick}@${s.node_id?.slice(0, 8)}`).join(', ') || '—' }} · replayed {{ x.at.replayed }} ops · history {{ x.at.history?.oldest_tick }}–{{ x.at.history?.newest_tick }}</span></div>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'pos', fmt: (r) => r.pos ? `${r.pos.x}, ${r.pos.y}` : '—', mono: true }, { key: 'data', fmt: (r) => JSON.stringify(r.data ?? r.state ?? {}), dim: true }]" :rows="list('at', 'entities')" empty="No entities at that tick" />
    </template>

    <template v-else-if="tab === 'view'">
      <div class="tools"><input class="in" type="number" v-model.number="cx" style="min-width:100px" /><input class="in" type="number" v-model.number="cy" style="min-width:100px" /><button class="btn p" @click="loadTab('view')">View cell</button><span class="dim" v-if="x.view">owner {{ x.view.owner ?? 'none' }} · tick {{ x.view.tick }}</span></div>
      <h3 style="margin-top:0">Owned <small>({{ list('view', 'owned').length }})</small></h3><JsonView :value="list('view', 'owned')" />
      <h3>Ghosts <small>({{ list('view', 'ghosts').length }})</small></h3><JsonView :value="list('view', 'ghosts')" />
      <h3>Globals <small>({{ list('view', 'globals').length }})</small></h3><JsonView :value="list('view', 'globals')" />
    </template>

    <template v-else-if="tab === 'config'"><JsonView :value="world.config" /></template>
  </div>

  <Drawer v-if="dlg === 'host'" title="Host a cell on this node" @close="dlg = ''"><div class="form">
    <div class="f"><label>Cell x</label><input class="in" type="number" v-model.number="form.x" /></div><div class="f"><label>Cell y</label><input class="in" type="number" v-model.number="form.y" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${A}/cells/${form.x}/${form.y}/host`), 'cell hosted here')">Host</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'rewind'" title="Rewind world" @close="dlg = ''"><div class="form">
    <p class="note">Replays the journal up to the given tick and discards everything after it. Simulators must re-attach.</p>
    <div class="f"><label>Tick</label><input class="in" type="number" v-model.number="form.tick" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn d" @click="ask(`Rewind ${w} to tick ${form.tick}?`) && act(() => post(`${S}/rewind`, { tick: form.tick }), 'rewound')">Rewind</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'spawn'" title="Spawn entity" @close="dlg = ''"><div class="form">
    <div class="f"><label>Kind</label><input class="in" v-model="form.kind" /></div>
    <div class="f"><label>x / y</label><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="form.x" /><input class="in" type="number" v-model.number="form.y" /></div></div>
    <div class="f"><label><input type="checkbox" v-model="form.persistent" /> persistent (journaled)</label></div>
    <div class="f"><label>Data (JSON)</label><textarea class="in" v-model="form.data" style="min-height:120px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${S}/spawn`, { kind: form.kind, pos: { x: form.x, y: form.y }, persistent: form.persistent, data: JSON.parse(form.data || '{}') }), 'spawned')">Spawn</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
