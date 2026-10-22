<script setup lang="ts">
// The cluster as the control plane sees it: membership (heartbeats + SWIM verdicts), the Raft
// directory, the QUIC bus and fan-out lanes, and where worlds and matches live. Rates come from
// this node's /metrics sampled once a second.
import { ref, onMounted, onBeforeUnmount, computed } from 'vue'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const live = ref(true); const dirKey = ref(''); const profiling = ref(false)
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = { error: e.message, status: e.status } } }
async function load() {
  await Promise.all([fx('nodes', '/v1/server/cluster/nodes'), fx('raft', '/v1/server/cluster/raft'), fx('mesh', '/v1/server/cluster/mesh_net'), fx('worlds', '/v1/admin/cluster/worlds'), fx('matches', '/v1/admin/cluster/matches'), sample()])
}
// ── /metrics: counters sampled into per-second rates ──
interface Sample { at: number; m: Map<string, number> }
let prev: Sample | null = null; const cur = ref<Sample | null>(null); const rate = ref<Map<string, number>>(new Map())
async function sample() {
  try {
    const r = await api<Response>('GET', '/metrics', undefined, { raw: true }); if (!r.ok) return
    const m = new Map<string, number>()
    for (const line of (await r.text()).split('\n')) { if (!line.startsWith('musubi_')) continue; const sp = line.lastIndexOf(' '); m.set(line.slice(0, sp), Number(line.slice(sp + 1))) }
    const s = { at: performance.now(), m }
    if (prev) { const dt = (s.at - prev.at) / 1000; const rr = new Map<string, number>(); for (const [k, v] of m) { const p = prev.m.get(k); if (p != null && dt > 0.4) rr.set(k, Math.max(0, (v - p) / dt)) } rate.value = rr }
    prev = cur.value; cur.value = s
  } catch { /* metrics may be served elsewhere */ }
}
// sum of a metric over series matching every given label=value
function sum(src: Map<string, number>, name: string, labels: Record<string, string> = {}) { let t = 0; for (const [k, v] of src) { if (!k.startsWith(name + '{') && k !== name) continue; if (Object.entries(labels).every(([l, val]) => k.includes(`${l}="${val}"`))) t += v } return t }
function series(src: Map<string, number>, name: string) { const out: { labels: Record<string, string>; v: number }[] = []; for (const [k, v] of src) { if (!k.startsWith(name + '{')) continue; const labels: Record<string, string> = {}; for (const m of k.slice(name.length + 1, -1).matchAll(/(\w+)="([^"]*)"/g)) labels[m[1]] = m[2]; out.push({ labels, v }) } return out }
const tot = computed(() => cur.value?.m ?? new Map<string, number>())
let timer: ReturnType<typeof setInterval> | null = null
function arm() { if (timer) clearInterval(timer); timer = live.value ? setInterval(load, 1000) : null }
onMounted(() => { load(); arm() }); onBeforeUnmount(() => { if (timer) clearInterval(timer) })
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); return r } catch (e: any) { toast.err(e) } finally { load() } }
const ask = (m: string) => window.confirm(m)
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
function ago(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : +new Date(v); const s = Math.round((Date.now() - d) / 1000); return s < 60 ? `${s}s ago` : s < 3600 ? `${Math.round(s / 60)}m ago` : s < 86400 ? `${(s / 3600).toFixed(1)}h ago` : `${Math.round(s / 86400)}d ago` }
const fmtN = (v?: number) => v == null ? '—' : v >= 1e6 ? (v / 1e6).toFixed(2) + ' M' : v >= 1e4 ? (v / 1e3).toFixed(1) + ' k' : v >= 100 || Number.isInteger(v) ? v.toFixed(0) : v.toFixed(1)
const mb = (b?: number) => b == null ? '—' : b >= 1048576 ? (b / 1048576).toFixed(1) + ' MB' : (b / 1024).toFixed(0) + ' KB'

// ── membership + placement ──
const byRaft = computed(() => { const m: Record<string, any> = {}; for (const n of x.value.nodes?.nodes ?? []) if (n.raft_id != null) m[String(n.raft_id)] = n; return m })
const name = (id?: string | null) => (id ? (x.value.nodes?.nodes ?? []).find((n: any) => n.id === id)?.name ?? '…' + String(id).slice(-8) : '—')
const raft = computed(() => x.value.raft ?? {})
const raftRole = (n: any) => !raft.value.enabled ? '—' : String(n.raft_id) === String(raft.value.leader) ? 'leader' : (raft.value.voters ?? []).map(String).includes(String(n.raft_id)) ? 'voter' : 'learner'
const nodes = computed(() => (x.value.nodes?.nodes ?? []).map((n: any) => ({ ...n, me: n.id === x.value.nodes?.self, raft: raftRole(n), worlds: (x.value.worlds?.worlds ?? []).filter((w: any) => w.node_id === n.id).length, matches: (x.value.matches?.matches ?? []).filter((m: any) => m.node_id === n.id).length })).sort((a: any, b: any) => (b.me ? 1 : 0) - (a.me ? 1 : 0) || a.name.localeCompare(b.name)))
const alive = computed(() => nodes.value.filter((n: any) => n.alive).length)
const leaderName = computed(() => raft.value.leader != null ? byRaft.value[String(raft.value.leader)]?.name ?? '…' + String(raft.value.leader).slice(-6) : '—')
const unplacedWorlds = computed(() => (x.value.worlds?.worlds ?? []).filter((w: any) => !w.node_id || !nodes.value.some((n: any) => n.id === w.node_id && n.alive)))

// ── bus & fan-out ──
const bus = computed(() => { const rows: any[] = []; for (const s of series(tot.value, 'musubi_bus_messages_total')) { const { transport, dir, ok } = s.labels; let r = rows.find((q) => q.transport === transport && q.dir === dir); if (!r) { r = { transport, dir, msgs: 0, failed: 0, bytes: sum(tot.value, 'musubi_bus_bytes_total', { transport, dir }), msgs_s: 0, bytes_s: sum(rate.value, 'musubi_bus_bytes_total', { transport, dir }) }; rows.push(r) } if (ok === 'false') r.failed += s.v; else r.msgs += s.v; r.msgs_s += sum(rate.value, 'musubi_bus_messages_total', { transport, dir, ok }) } return rows })
const busTx = computed(() => bus.value.filter((b) => b.dir === 'tx').reduce((a, b) => ({ msgs: a.msgs + b.msgs_s, bytes: a.bytes + b.bytes_s }), { msgs: 0, bytes: 0 }))
const busRx = computed(() => bus.value.filter((b) => b.dir === 'rx').reduce((a, b) => ({ msgs: a.msgs + b.msgs_s, bytes: a.bytes + b.bytes_s }), { msgs: 0, bytes: 0 }))
const lanes = computed(() => { const names = new Set<string>(); for (const s of [...series(tot.value, 'musubi_fanout_published_total'), ...series(tot.value, 'musubi_fanout_queue_depth')]) names.add(s.labels.lane); return [...names].sort().map((lane) => { const kinds = series(rate.value, 'musubi_fanout_published_total').filter((s) => s.labels.lane === lane).sort((a, b) => b.v - a.v); const ms = (phase: string) => { const c = sum(rate.value, 'musubi_fanout_batch_ms_count', { lane, phase }); return c ? sum(rate.value, 'musubi_fanout_batch_ms_sum', { lane, phase }) / c : null }; return { lane, published: sum(tot.value, 'musubi_fanout_published_total', { lane }), rate: sum(rate.value, 'musubi_fanout_published_total', { lane }), depth: sum(tot.value, 'musubi_fanout_queue_depth', { lane }), serialize: ms('serialize'), send: ms('send'), top: kinds.filter((k) => k.v > 0).slice(0, 3).map((k) => `${k.labels.kind} ${fmtN(k.v)}/s`).join(' · ') } }) })
const sessions = computed(() => sum(tot.value, 'musubi_realtime_sessions'))

async function lookup() { const k = dirKey.value.trim(); if (!k) return; try { x.value.dir = await get(`/v1/server/cluster/directory/${encodeURIComponent(k)}`) } catch (e: any) { x.value.dir = { key: k, error: e.message } } }
async function profile() {
  profiling.value = true
  try { const r = await api<Response>('GET', '/v1/admin/debug/profile', undefined, { raw: true }); if (!r.ok) throw new Error(`HTTP ${r.status}`); const url = URL.createObjectURL(await r.blob()); window.open(url, '_blank'); toast.show('flame graph opened') } catch (e: any) { toast.err(e) } finally { profiling.value = false }
}
</script>
<template>
  <div class="stats">
    <div class="stat"><div class="k">Nodes alive</div><div class="v" :class="{ bad: alive < nodes.length }">{{ alive }}<small>of {{ nodes.length }} · {{ new Set(nodes.map((n: any) => n.region)).size }} regions</small></div></div>
    <div class="stat"><div class="k">Raft directory</div><div class="v">{{ raft.enabled ? leaderName : 'off' }}<small v-if="raft.enabled">{{ raft.is_leader ? 'this node leads' : 'leader' }} · {{ (raft.voters ?? []).length }} voters</small><small v-else>single node — DB leases</small></div></div>
    <div class="stat"><div class="k">Bus out / in</div><div class="v">{{ fmtN(busTx.msgs) }}<small>msg/s · {{ mb(busTx.bytes) }}/s out · {{ fmtN(busRx.msgs) }} msg/s in</small></div></div>
    <div class="stat"><div class="k">Realtime sessions</div><div class="v">{{ fmtN(sessions) }}<small>on this node</small></div></div>
    <div class="stat"><div class="k">Worlds / matches</div><div class="v">{{ (x.worlds?.worlds ?? []).length }}<small>{{ (x.worlds?.local ?? []).length }} homed here · {{ (x.matches?.matches ?? []).length }} matches, {{ x.matches?.local ?? 0 }} here</small><small v-if="unplacedWorlds.length" class="warn">{{ unplacedWorlds.length }} awaiting adoption</small></div></div>
  </div>

  <div class="card">
    <h2>Nodes <small>({{ nodes.length }})</small><label class="dim" style="float:right;font-size:12px;font-weight:400"><input type="checkbox" v-model="live" /> live</label></h2>
    <p class="err" v-if="x.nodes?.error">{{ x.nodes.error }}</p>
    <DataTable :cols="[{ key: 'name', fmt: (r) => r.name + (r.me ? ' (this)' : '') }, { key: 'state', fmt: (r) => r.alive ? r.state : 'dead' }, { key: 'raft', fmt: (r) => r.raft }, { key: 'roles', fmt: (r) => (r.roles ?? []).join(' · ') }, { key: 'region', fmt: (r) => `${r.region}${r.group && r.group !== 'default' ? ' / ' + r.group : ''}` }, { key: 'worlds', label: 'worlds homed' }, { key: 'matches' }, { key: 'heartbeat_at', label: 'heartbeat', fmt: (r) => ago(r.heartbeat_at), dim: true }, { key: 'started_at', label: 'up since', fmt: (r) => ts(r.started_at), dim: true }, { key: 'version' }, { key: 'advertise', label: 'http / bus', mono: true, fmt: (r) => `${r.advertise}${r.bus_addr ? ' · ' + r.bus_addr : ' · no QUIC bus (PG notify)'}` }]" :rows="nodes" empty="No nodes registered">
      <template #actions="{ row }"><button class="btn s d" v-if="!row.me" @click="ask(`Drain ${row.name}? Its worlds and leases move to other nodes.`) && act(() => post(`/v1/admin/cluster/nodes/${row.id}/drain`), 'drain requested')">Drain</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">State is the SWIM verdict (alive · suspect · dead) confirmed by the heartbeat row; a dead node's worlds and matches are adopted by the others. A node without a bus address falls back to Postgres NOTIFY for every inter-node message — expect it to be slow.</p>
    <template v-if="unplacedWorlds.length"><h3 class="warn">Worlds awaiting adoption <small>({{ unplacedWorlds.length }})</small></h3><div><span v-for="w in unplacedWorlds" :key="w.name" class="pill a" style="margin:0 6px 6px 0">{{ w.name }}</span></div></template>
  </div>

  <div class="card">
    <h2>Bus & fan-out <small>(this node)</small></h2>
    <h3 style="margin-top:0">Transport</h3>
    <DataTable :cols="[{ key: 'transport' }, { key: 'dir', fmt: (r) => r.dir === 'tx' ? 'out' : 'in' }, { key: 'msgs_s', label: 'msg/s', fmt: (r) => fmtN(r.msgs_s) }, { key: 'bytes_s', label: 'bytes/s', fmt: (r) => mb(r.bytes_s) }, { key: 'msgs', label: 'total msgs', fmt: (r) => fmtN(r.msgs), dim: true }, { key: 'bytes', label: 'total bytes', fmt: (r) => mb(r.bytes), dim: true }, { key: 'failed', fmt: (r) => r.failed ? fmtN(r.failed) : '0' }]" :rows="bus" empty="No bus traffic yet — single node, or the QUIC bus is not configured (node.bus_listen)" />
    <h3>Lanes</h3>
    <DataTable :cols="[{ key: 'lane' }, { key: 'rate', label: 'published/s', fmt: (r) => fmtN(r.rate) }, { key: 'depth', label: 'queue depth', fmt: (r) => String(r.depth) }, { key: 'serialize', label: 'serialize ms', fmt: (r) => r.serialize == null ? '—' : r.serialize.toFixed(2) }, { key: 'send', label: 'send ms', fmt: (r) => r.send == null ? '—' : r.send.toFixed(2) }, { key: 'top', label: 'top kinds now', dim: true, fmt: (r) => r.top || 'idle' }, { key: 'published', label: 'total', fmt: (r) => fmtN(r.published), dim: true }]" :rows="lanes" empty="No lanes active" />
    <p class="note" style="margin-top:10px">Lanes are FIFO per priority (control before session before user); a growing queue depth means the bus cannot drain this node's publications — look at send ms and the peer's state first.</p>
  </div>

  <div class="card">
    <h2>Raft directory</h2>
    <div class="grid" v-if="raft.enabled">
      <div><Kv k="Leader" :v="leaderName" /><Kv k="This node" :v="raft.is_leader ? 'leader' : (raft.voters ?? []).map(String).includes(String(raft.raft_id)) ? 'voter' : 'learner'" /></div>
      <div><Kv k="Voters" :v="(raft.voters ?? []).map((v: any) => byRaft[String(v)]?.name ?? '…' + String(v).slice(-6)).join(', ')" /><Kv k="Replication" :v="raft.is_leader ? 'this node replicates the directory to the voters' : 'follows the leader'" /></div>
      <div><Kv k="Quorum" :v="`${Math.floor((raft.voters ?? []).length / 2) + 1} of ${(raft.voters ?? []).length}`" /><Kv k="Dead voters" :v="(raft.voters ?? []).filter((v: any) => byRaft[String(v)] && !byRaft[String(v)].alive).length || 'none'" /></div>
    </div>
    <p class="dim" v-else>Raft is off: one node, or <code>[cluster]</code> without a bus. World cells and their hosts are then recorded in Postgres (<code>musubi.world_cell_hosts</code>) instead of the replicated directory.</p>
    <div class="tools" style="margin-top:14px"><input class="in" v-model="dirKey" placeholder="directory key, e.g. world:&lt;name&gt;" style="min-width:320px" @keyup.enter="lookup" /><button class="btn" :disabled="!dirKey.trim()" @click="lookup">Look up</button><span class="dim">reads through the leader (fresh)</span></div>
    <JsonView v-if="x.dir" :value="x.dir" />
  </div>

  <div class="card">
    <h2>This node</h2>
    <div class="tools"><button class="btn" :disabled="profiling" @click="profile">{{ profiling ? 'profiling ~10 s…' : 'CPU flame graph' }}</button><router-link class="btn" to="/cluster/logs">Logs</router-link><router-link class="btn" to="/cluster/loadtests">Load tests</router-link><span class="dim">mesh network: {{ x.mesh?.enabled ? `on · ${(x.mesh.peers ?? []).length} peers` : 'off' }} · <router-link to="/cluster/mesh_net">details</router-link></span></div>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
