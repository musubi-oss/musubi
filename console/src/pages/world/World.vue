<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import CellMap, { type MapCell } from '../../components/CellMap.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const w = computed(() => String(route.params.w))
const S = computed(() => `/v1/server/worlds/${w.value}`), A = computed(() => `/v1/admin/worlds/${w.value}`)
const world = ref<any>(null); const err = ref(''); const x = ref<Record<string, any>>({})
const tab = ref(String(route.query.tab ?? 'overview'))
const tabs: [string, string][] = [['overview', 'overview'], ['cells', 'cells'], ['sims', 'simulators & leases'], ['entities', 'entities'], ['handovers', 'handovers'], ['journal', 'journal'], ['at', 'time travel'], ['view', 'cell view'], ['config', 'config']]
const dlg = ref<'' | 'host' | 'rewind' | 'spawn'>(''); const form = ref<any>({})
const tick = ref(0); const cx = ref(0); const cy = ref(0); const entQ = ref(''); const jFrom = ref<number | null>(null)
const hoEntity = ref(''); const hoLimit = ref(500); const hoGroup = ref(true)
const live = ref(true); const nodes = ref<Record<string, string>>({})

// ── rates: ticks/s from the tick counter, handovers & frames/s from this node's /metrics ──
const rates = ref<{ ticks: number; handovers: number; frames: number } | null>(null)
let prev: { at: number; tick: number; ho: number; fr: number } | null = null
function metric(text: string, name: string) { const re = new RegExp(`^${name}\\{[^}]*world="${w.value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}"[^}]*\\}\\s+([0-9.e+]+)`, 'm'); const m = text.match(re); return m ? Number(m[1]) : 0 }
async function sample() {
  let ho = 0, fr = 0
  try { const r = await api<Response>('GET', '/metrics', undefined, { raw: true }); const t = r.ok ? await r.text() : ''; ho = metric(t, 'musubi_world_handovers_total'); fr = metric(t, 'musubi_world_frames_total') } catch { /* metrics may live on another port */ }
  const now = performance.now(), tk = world.value?.tick ?? 0
  if (prev && now - prev.at > 400) { const dt = (now - prev.at) / 1000; rates.value = { ticks: (tk - prev.tick) / dt, handovers: (ho - prev.ho) / dt, frames: (fr - prev.fr) / dt } }
  prev = { at: now, tick: tk, ho, fr }
}

async function load() { err.value = ''; try { world.value = await get(S.value); await sample() } catch (e: any) { err.value = e.message } }
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = e.status === 404 ? null : { error: e.message } } }
function loadTab(t: string) {
  if (t === 'entities' || t === 'sims' || t === 'overview' || t === 'cells') fx('entities', `${A.value}/entities` + qs({ q: t === 'entities' && entQ.value ? entQ.value : undefined, limit: 5000 }))
  if (t === 'handovers') fx('handovers', `${A.value}/handovers` + qs({ entity: hoEntity.value.trim() || undefined, limit: hoLimit.value }))
  if (t === 'journal') fx('history', `${A.value}/history` + qs({ from_tick: jFrom.value ?? undefined, limit: 200 }))
  if (t === 'at') fx('at', `${A.value}/at` + qs({ tick: tick.value }))
  if (t === 'view') fx('view', `${S.value}/view/${cx.value}/${cy.value}`)
}
watch(tab, (t) => { router.replace({ query: { ...route.query, tab: t } }); loadTab(t) }, { immediate: true })
let timer: ReturnType<typeof setInterval> | null = null
function arm() { if (timer) clearInterval(timer); timer = live.value ? setInterval(() => { load(); if (['overview', 'cells', 'sims', 'handovers'].includes(tab.value)) loadTab(tab.value) }, 1000) : null }
watch(live, arm)
onMounted(async () => { load(); arm(); try { const n = await get('/v1/server/cluster/nodes'); for (const z of n?.nodes ?? []) nodes.value[z.id] = z.name } catch { /* single node */ } })
onBeforeUnmount(() => { if (timer) clearInterval(timer) })
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; await load(); loadTab(tab.value) } catch (e: any) { toast.err(e) } }
const ask = (m: string) => window.confirm(m)
const list = (k: string, sub?: string) => { const v = x.value[k]; if (!v || v.error) return []; const a = sub ? v[sub] : Array.isArray(v) ? v : Object.values(v).find(Array.isArray); return Array.isArray(a) ? a : [] }
function cellKey(c: any) { if (!c) return '—'; return c.id ?? c.key ?? (c.x != null ? `${c.x},${c.y}${c.z != null ? ',' + c.z : ''}${c.level != null ? ' L' + c.level : ''}` : JSON.stringify(c)) }
const sameCell = (a: any, b: any) => a && b && a.x === b.x && a.y === b.y

// ── cells joined with owners, entity counts, leases, orphan/cold state ──
const entities = computed<any[]>(() => list('entities', 'entities'))
const leases = computed<any[]>(() => list('entities', 'leases'))
const cells = computed(() => {
  const counts = new Map<string, { n: number; transit: number }>()
  for (const e of entities.value) { const k = cellKey(e.cell); const c = counts.get(k) ?? { n: 0, transit: 0 }; c.n++; if (e.transit || e.remote_to) c.transit++; counts.set(k, c) }
  const orphans = new Set((world.value?.orphaned ?? []).map(cellKey))
  const hot = (world.value?.cells ?? []).map((c: any) => { const [cell, owner] = Array.isArray(c) ? c : [c, c.owner]; const k = cellKey(cell); const lease = leases.value.find((l) => sameCell(l.cell, cell)); return { ...cell, id: k, owner: owner ?? lease?.sim ?? null, lease: lease?.sim ?? null, orphaned: orphans.has(k), cold: false, entities: counts.get(k)?.n ?? 0, transit: counts.get(k)?.transit ?? 0 } })
  const cold = (world.value?.cold_cells ?? []).map((c: any) => ({ ...c, id: cellKey(c), owner: null, lease: null, orphaned: false, cold: true, entities: 0, transit: 0 }))
  return [...hot, ...cold] as (MapCell & { id: string; entities: number; transit: number; lease: string | null })[]
})
const mapEntities = computed(() => entities.value.map((e) => ({ x: e.pos?.x ?? e.x ?? 0, y: e.pos?.y ?? e.y ?? 0, transit: !!(e.transit || e.remote_to), cell: e.cell })))
const simRows = computed(() => (world.value?.sims ?? []).map((s: any) => { const id = typeof s === 'string' ? s : s.id; return { ...(typeof s === 'string' ? { id } : s), cells: cells.value.filter((c) => c.owner === id).length, entities: cells.value.filter((c) => c.owner === id).reduce((a, c) => a + c.entities, 0), lag: s.acked_tick != null && world.value ? world.value.tick - s.acked_tick : null } }))
// UUIDv7 ids share their time prefix, so the tail is the telling part
const simCount = computed(() => (world.value?.sims ?? []).length || new Set(leases.value.map((l) => l.sim)).size)
const simName = (id?: string | null) => (id ? nodes.value[id] ?? '…' + id.slice(-8) : '—')
const homeNote = computed(() => { const e = x.value.entities; if (!e) return 'home unknown'; if (e.hosted_only) return e.home_node ? `home is ${simName(e.home_node)}` : 'home pending adoption'; return 'this node is home' })
function pickCell(c: MapCell) { cx.value = c.x; cy.value = c.y; tab.value = 'view' }

// ── handovers: the journal trail per entity; anything but the happy path is flagged ──
const okPaths = new Set(['local', 'remote', 'forced', 'accepted', 'completed', 'forced_take', 'have_it'])
const steps = computed<any[]>(() => list('handovers', 'steps'))
const trails = computed(() => {
  const open = new Map<string, any[]>(); const all: [string, any[]][] = []
  for (const s of steps.value) { let a = open.get(s.entity); if (!a || s.event === 'started') { a = []; open.set(s.entity, a); all.push([s.entity, a]) } a.push(s) }
  return all.map(([entity, ss]) => { const last = ss[ss.length - 1]; const bad = ss.some((s) => !okPaths.has(s.path)); return { entity, steps: ss, first: ss[0], last, bad, open: last.event !== 'ended', from: ss[0].detail?.split(' -> ')[0]?.slice(0, 8), ms: last.ms - ss[0].ms } }).sort((a, b) => b.last.ms - a.last.ms)
})
const hoStats = computed(() => ({ entities: trails.value.length, open: trails.value.filter((t) => t.open).length, bad: trails.value.filter((t) => t.bad).length }))
const fmtMs = (ms: number) => new Date(ms).toISOString().slice(11, 23)
function openDlg(k: typeof dlg.value) { dlg.value = k; form.value = k === 'host' ? { x: 0, y: 0 } : k === 'rewind' ? { tick: Math.max(0, (world.value?.tick ?? 0) - 100) } : { kind: 'npc', x: 0, y: 0, persistent: true, data: '{}' } }
</script>
<template>
  <div class="crumb"><router-link to="/world">Worlds</router-link> › <span class="m">{{ w }}</span>
    <span style="margin-left:auto;display:flex;gap:8px;align-items:center">
      <label class="dim" style="font-size:12px;margin-right:6px"><input type="checkbox" v-model="live" /> live</label>
      <button class="btn s" @click="act(() => post(`${A}/snapshot`), 'snapshot written')">Snapshot</button>
      <button class="btn s" @click="act(async () => { const r = await post(`${A}/verify`); x.verify = r }, 'verified')">Verify</button>
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
      <div class="stats" style="margin-bottom:16px">
        <div class="stat"><div class="k">Tick</div><div class="v">{{ world.tick }}<small v-if="rates">{{ rates.ticks.toFixed(1) }} /s</small></div></div>
        <div class="stat"><div class="k">Entities</div><div class="v">{{ world.entities }}<small v-if="mapEntities.some((e) => e.transit)" class="warn">{{ mapEntities.filter((e) => e.transit).length }} in transit</small></div></div>
        <div class="stat"><div class="k">Cells hot / cold</div><div class="v">{{ (world.cells ?? []).length }}<small>/ {{ (world.cold_cells ?? []).length }} cold</small><small v-if="(world.orphaned ?? []).length" class="bad">{{ world.orphaned.length }} orphaned</small></div></div>
        <div class="stat"><div class="k">Simulators</div><div class="v">{{ simCount }}<small>{{ leases.length }} leases</small></div></div>
        <div class="stat"><div class="k">Handovers</div><div class="v">{{ rates ? rates.handovers.toFixed(1) : '—' }}<small>/s on this node</small></div></div>
        <div class="stat"><div class="k">Players</div><div class="v">{{ world.subscribers }}<small v-if="rates">{{ rates.frames.toFixed(0) }} frames/s</small></div></div>
      </div>
      <CellMap :cells="cells" :entities="mapEntities" :cell-size="world.config?.cell_size ?? 1" :names="nodes" :selected="null" @pick="pickCell" />
      <div class="note" style="margin-top:8px">Hot cells tinted by simulator, hibernated cells dimmed, orphaned outlined red; dots are entities (amber while changing cells). Entities shown are this node's shard ({{ homeNote }}). Click a cell for its view.</div>
      <div class="grid" style="margin-top:18px">
        <div><Kv k="Cell size" :v="world.config?.cell_size" /><Kv k="Partition" :v="world.config?.three_d ? 'octree' : world.config?.max_depth > 0 ? `quadtree, depth ≤ ${world.config.max_depth}` : 'fixed grid'" /><Kv k="Ghost margin / hysteresis" :v="`${world.config?.ghost_margin} / ${world.config?.hysteresis}`" /><Kv k="Replicas per cell" :v="world.config?.replicas" /></div>
        <div><Kv k="Lease TTL / handover timeout" :v="`${world.config?.lease_ttl_ticks} / ${world.config?.handover_timeout_ticks} ticks`" /><Kv k="Hibernate after" :v="world.config?.hibernate_after_ticks ? world.config.hibernate_after_ticks + ' ticks' : 'never'" /><Kv k="Time dilation" :v="world.config?.tidi_after_ticks ? `after ${world.config.tidi_after_ticks} late ticks, floor ×${world.config.tidi_min}` : 'off'" /><Kv k="Admission queue TTL" :v="world.config?.queue_ttl_ticks + ' ticks'" /></div>
        <div><Kv k="Client budget" :v="`${world.config?.client_budget_bytes} B, min ${world.config?.client_min_delta_ticks} ticks`" /><Kv k="Keyframe every" :v="world.config?.keyframe_ticks + ' ticks'" /><Kv k="Contacts co-locate" :v="`after ${world.config?.contact_colocate_after_ticks} ticks, pin ${world.config?.contact_colocate_ttl_ticks}`" /><Kv k="History kept" :v="world.config?.history_ticks + ' ticks'" /></div>
      </div>
      <template v-if="x.verify"><h3>Last verify <small :class="x.verify.ok === false ? 'bad' : 'ok'">{{ x.verify.ok === false ? 'problems found' : 'ok' }}</small></h3><JsonView :value="x.verify" /></template>
    </template>

    <template v-else-if="tab === 'cells'">
      <h3 style="margin-top:0">Hot cells <small>({{ cells.filter((c) => !c.cold).length }})</small></h3>
      <DataTable :cols="[{ key: 'id', label: 'cell', mono: true }, { key: 'owner', label: 'simulator', mono: true, fmt: (r) => r.owner ? simName(r.owner) : r.orphaned ? 'orphaned' : '—' }, { key: 'entities' }, { key: 'transit', label: 'in transit' }, { key: 'lease', fmt: (r) => r.lease ? 'leased' : 'no lease' }, { key: 'replica', fmt: (r) => { const n = (world.replicas ?? []).filter((p: any) => sameCell(p.cell, r)).length; return n ? `${n} standby` : '—' } }]" :rows="cells.filter((c) => !c.cold)" pick @pick="pickCell" />
      <h3>Hibernated cells <small>({{ (world.cold_cells ?? []).length }})</small></h3>
      <div class="dim" v-if="!(world.cold_cells ?? []).length">None — every cell with persistent entities is in memory</div>
      <div v-else><span v-for="c in world.cold_cells" :key="cellKey(c)" class="pill" style="margin:0 6px 6px 0">{{ cellKey(c) }}</span></div>
      <h3>Replicas <small>(standby copies of cells hosted here)</small></h3>
      <DataTable :cols="[{ key: 'cell', fmt: (r) => cellKey(r.cell), mono: true }, { key: 'node', fmt: (r) => simName(r.node), mono: true }, { key: 'tick', label: 'acked tick' }, { key: 'lag', fmt: (r) => String(world.tick - r.tick) }]" :rows="world.replicas ?? []" empty="No replicas — `replicas` is 0 or this is the only sim node" />
      <h3>Mirrors <small>(cells hosted elsewhere that this node follows)</small></h3>
      <DataTable :cols="[{ key: 'cell', fmt: (r) => cellKey(r.cell), mono: true }, { key: 'node', label: 'host', fmt: (r) => simName(r.node), mono: true }, { key: 'tick', label: 'host tick' }]" :rows="world.mirrors ?? []" empty="No mirrors" />
    </template>

    <template v-else-if="tab === 'sims'">
      <h3 style="margin-top:0">Simulators <small>({{ simRows.length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'cells' }, { key: 'entities' }, { key: 'capacity', fmt: (r) => r.capacity ? String(r.capacity) : 'unbounded' }, { key: 'last_seen_tick', label: 'last seen (tick)' }, { key: 'acked_tick', label: 'acked tick', fmt: (r) => r.acked_tick ?? 'never' }, { key: 'lag', label: 'behind (ticks)', fmt: (r) => r.lag == null ? '—' : String(r.lag) }]" :rows="simRows" empty="No simulator attached — cells stay orphaned until one leases them" />
      <h3>Leases <small>({{ leases.length }})</small></h3>
      <DataTable :cols="[{ key: 'cell', fmt: (r) => cellKey(r.cell), mono: true }, { key: 'sim', mono: true }, { key: 'entities', fmt: (r) => String(cells.find((c) => sameCell(c, r.cell))?.entities ?? 0) }]" :rows="leases" empty="No leases" />
      <div class="note" style="margin-top:10px">{{ homeNote }} · this shard from node {{ simName(x.entities?.node_id) }}<span v-if="x.entities?.hosted_only"> · hosted-only (home is elsewhere)</span></div>
    </template>

    <template v-else-if="tab === 'entities'">
      <div class="tools"><input class="in" v-model="entQ" placeholder="filter id" @keyup.enter="loadTab('entities')" /><button class="btn" @click="loadTab('entities')">Refresh</button><span class="dim">tick {{ x.entities?.tick }} · {{ entities.length }} on this node's shard</span></div>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'cell', fmt: (r) => cellKey(r.cell), mono: true }, { key: 'pos', fmt: (r) => r.pos ? `${r.pos.x.toFixed(1)}, ${r.pos.y.toFixed(1)}${r.pos.z != null ? ', ' + r.pos.z.toFixed(1) : ''}` : '—', mono: true }, { key: 'persistent', fmt: (r) => r.persistent ? 'journaled' : 'volatile' }, { key: 'transit', fmt: (r) => r.remote_to ? `→ node ${simName(r.remote_to)}` : r.transit ? `→ ${cellKey(r.transit.to ?? r.transit)}` : '—' }]" :rows="entities" pick @pick="(r) => { x.ent = r; hoEntity = r.id }" />
      <Drawer v-if="x.ent" title="Entity" @close="x.ent = null"><JsonView :value="x.ent" />
        <div class="form"><div class="row"><button class="btn" @click="tab = 'handovers'; x.ent = null">Handover trail</button><button class="btn d" @click="ask('Despawn this entity?') && act(() => post(`${S}/despawn`, { id: x.ent.id }), 'despawned').then(() => (x.ent = null))">Despawn</button></div></div></Drawer>
    </template>

    <template v-else-if="tab === 'handovers'">
      <div class="tools"><input class="in" v-model="hoEntity" placeholder="entity id (empty = all)" style="min-width:320px" @keyup.enter="loadTab('handovers')" /><select class="in" v-model.number="hoLimit" @change="loadTab('handovers')"><option :value="200">last 200 steps</option><option :value="500">last 500</option><option :value="2000">last 2 000</option><option :value="10000">last 10 000</option></select><button class="btn" @click="loadTab('handovers')">Load</button>
        <label class="dim" style="font-size:12px"><input type="checkbox" v-model="hoGroup" /> group by entity</label>
        <span class="dim" style="margin-left:auto">{{ steps.length }} steps · {{ hoStats.entities }} handovers · <span :class="{ warn: hoStats.open }">{{ hoStats.open }} in flight</span> · <span :class="{ bad: hoStats.bad }">{{ hoStats.bad }} off the happy path</span></span></div>
      <p class="note">Every step of a change of owner, oldest first, from every node's durable journal plus this node's unflushed tail: <code>started</code> (the cell border crossing), <code>shipped</code> (remote / forced on offload), <code>received</code> (what the receiver did), <code>ack</code> (the new simulator), <code>ended</code>. Anything but the happy path is marked.</p>
      <template v-if="hoGroup">
        <table><thead><tr><th style="width:230px">Entity</th><th style="width:90px">Ticks</th><th>Trail</th><th style="width:110px">Took</th><th style="width:130px">Last step</th></tr></thead><tbody>
          <tr v-for="t in trails" :key="t.entity + t.first.ms" class="row" @click="hoEntity = t.entity; hoGroup = false; loadTab('handovers')">
            <td class="m">…{{ t.entity.slice(-12) }}</td><td class="dim">{{ t.first.tick }}<span v-if="t.last.tick !== t.first.tick"> → {{ t.last.tick }}</span></td>
            <td><span v-for="(s, i) in t.steps" :key="i" class="hstep" :class="{ bad: !okPaths.has(s.path) }" :title="s.detail">{{ s.event }}<small>/{{ s.path }}</small></span><span v-if="t.open" class="dim" style="font-size:12px">…</span></td>
            <td class="dim">{{ t.ms }} ms</td><td class="dim">{{ fmtMs(t.last.ms) }}</td>
          </tr>
          <tr v-if="!trails.length"><td colspan="5" class="empty">No handovers recorded{{ hoEntity ? ' for this entity' : '' }}</td></tr></tbody></table>
      </template>
      <table v-else><thead><tr><th style="width:110px">Time</th><th style="width:80px">Tick</th><th style="width:130px">Entity</th><th style="width:90px">Event</th><th style="width:130px">Path</th><th style="width:110px">Node</th><th>Detail</th></tr></thead><tbody>
        <tr v-for="(s, i) in steps" :key="i"><td class="dim">{{ fmtMs(s.ms) }}</td><td class="dim">{{ s.tick }}</td><td class="m">…{{ s.entity.slice(-12) }}</td><td>{{ s.event }}</td><td :class="okPaths.has(s.path) ? '' : 'bad'">{{ s.path }}</td><td class="m dim">{{ simName(s.node) }}</td><td class="dim" style="word-break:break-all">{{ s.detail }}</td></tr>
        <tr v-if="!steps.length"><td colspan="7" class="empty">No handovers recorded{{ hoEntity ? ' for this entity' : '' }}</td></tr></tbody></table>
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
      <div class="tools"><input class="in" type="number" v-model.number="cx" style="min-width:100px" /><input class="in" type="number" v-model.number="cy" style="min-width:100px" /><button class="btn p" @click="loadTab('view')">View cell</button><span class="dim" v-if="x.view">owner {{ x.view.owner ? simName(x.view.owner) : 'none' }} · tick {{ x.view.tick }}<span v-if="x.view.time_scale != null && x.view.time_scale !== 1" class="warn"> · time ×{{ x.view.time_scale }}</span></span></div>
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
<style scoped>
.hstep{display:inline-block;font-size:12px;padding:1px 6px;margin:1px 4px 1px 0;border:1px solid var(--line2);border-radius:4px;color:var(--fg2)}
.hstep small{color:var(--fg3)}.hstep.bad{color:var(--red);border-color:rgba(239,90,90,.45)}.hstep.bad small{color:var(--red)}
</style>
