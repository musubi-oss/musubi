<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const ip = ref(''); const cfgText = ref('')
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = { error: e.message, status: e.status } } }
async function load() {
  await Promise.all([fx('nodes', '/v1/server/cluster/nodes'), fx('raft', '/v1/server/cluster/raft'), fx('mesh', '/v1/server/cluster/mesh_net'), fx('geoip', '/v1/server/geoip'), fx('sinks', '/v1/server/analytics/sinks'), fx('chunking', '/v1/server/builds/chunking'), fx('kpi', '/v1/server/analytics/kpi/active_users'), fx('logs', '/v1/admin/logs/nodes')])
  if (x.value.mesh?.enabled) { try { const r: Response = await get('/v1/server/cluster/mesh_net/config?format=quick') as any; cfgText.value = typeof r === 'string' ? r : JSON.stringify(r) } catch { cfgText.value = '' } }
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); return r } catch (e: any) { toast.err(e) } finally { load() } }
const ask = (m: string) => window.confirm(m)
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
function ago(v: any) { if (!v) return '—'; const s = Math.round((Date.now() - +new Date(v)) / 1000); return s < 60 ? `${s}s ago` : s < 3600 ? `${Math.round(s / 60)}m ago` : `${(s / 3600).toFixed(1)}h ago` }
const nodes = computed(() => (x.value.nodes?.nodes ?? []).map((n: any) => ({ ...n, me: n.id === x.value.nodes?.self })))
const alive = computed(() => nodes.value.filter((n: any) => n.alive).length)
async function lookup() { try { x.value.lookup = await get(`/v1/server/geoip/lookup/${encodeURIComponent(ip.value)}`) } catch (e: any) { x.value.lookup = { error: e.message } } }
const mb = (b: number) => b == null ? '—' : (b / 1048576).toFixed(1) + ' MB'
</script>
<template>
  <div class="stats">
    <div class="stat"><div class="k">Nodes alive</div><div class="v" :class="{ bad: alive < nodes.length }">{{ alive }}<small>of {{ nodes.length }}</small></div></div>
    <div class="stat"><div class="k">Raft</div><div class="v">{{ x.raft?.enabled ? (x.raft.is_leader ? 'leader' : 'follower') : 'off' }}<small v-if="x.raft?.enabled">leader {{ x.raft.leader ?? '—' }} · {{ (x.raft.voters ?? []).length }} voters</small><small v-else>single-node / DB leases</small></div></div>
    <div class="stat"><div class="k">Private mesh</div><div class="v">{{ x.mesh?.enabled ? 'on' : 'off' }}<small v-if="x.mesh?.enabled">{{ x.mesh.address }} · {{ (x.mesh.peers ?? []).length }} peers · {{ x.mesh.applied ? 'applied' : 'config only' }}</small><small v-else>[mesh_net] enabled = false</small></div></div>
    <div class="stat"><div class="k">GeoIP</div><div class="v" :class="{ warn: x.geoip?.enabled && x.geoip?.stale }">{{ !x.geoip?.enabled ? 'off' : x.geoip.loaded ? (x.geoip.stale ? 'stale' : 'loaded') : 'no database' }}<small v-if="x.geoip?.enabled">{{ x.geoip.edition }} · {{ x.geoip.build_age_days ?? '?' }} d old · {{ x.geoip.lookups }} lookups</small></div></div>
    <div class="stat"><div class="k">Active users</div><div class="v">{{ x.kpi?.dau ?? x.kpi?.active ?? x.kpi?.today ?? '—' }}<small>DAU · {{ x.kpi?.wau ?? '—' }} WAU · {{ x.kpi?.mau ?? '—' }} MAU</small></div></div>
  </div>

  <div class="card">
    <h2>Nodes <small>({{ nodes.length }})</small></h2>
    <DataTable :cols="[{ key: 'name', fmt: (r) => r.name + (r.me ? ' (this)' : '') }, { key: 'roles', fmt: (r) => (r.roles ?? []).join(' · ') }, { key: 'region' }, { key: 'group' }, { key: 'state', fmt: (r) => r.alive ? r.state : 'dead' }, { key: 'heartbeat_at', label: 'heartbeat', fmt: (r) => ago(r.heartbeat_at), dim: true }, { key: 'started_at', label: 'started', fmt: (r) => ts(r.started_at), dim: true }, { key: 'version' }, { key: 'advertise', mono: true }, { key: 'wg_addr', label: 'mesh addr', mono: true }]" :rows="nodes">
      <template #actions="{ row }"><button class="btn s d" v-if="!row.me" @click="ask(`Drain ${row.name}? Its worlds and leases move to other nodes.`) && act(() => post(`/v1/admin/cluster/nodes/${row.id}/drain`), 'drain requested')">Drain</button></template>
    </DataTable>
  </div>

  <div class="card" v-if="x.mesh?.enabled">
    <h2>Private mesh (WireGuard)</h2>
    <div class="tools"><button class="btn" @click="act(() => post('/v1/server/cluster/mesh_net/refresh'), 'peers refreshed')">Refresh peers</button><button class="btn d" @click="ask('Rotate this node\'s key? Peers pick it up within one cycle.') && act(() => post('/v1/server/cluster/mesh_net/rotate'), 'key rotated')">Rotate key</button></div>
    <div class="grid">
      <div><Kv k="Address" :v="x.mesh.address" mono copy /><Kv k="Interface" :v="x.mesh.interface" mono /><Kv k="Public key" :v="x.mesh.public_key" mono copy /></div>
      <div><Kv k="Endpoint" :v="x.mesh.endpoint || 'behind NAT'" mono /><Kv k="Applied" :v="x.mesh.applied ? 'yes (kernel interface)' : x.mesh.tools_missing ? 'no — wg/ip missing' : 'no'" /><Kv k="Generation" :v="x.mesh.generation" /></div>
      <div><Kv k="Last error" :v="x.mesh.last_error" /><Kv k="Bus on mesh" :v="x.mesh.bind_bus" /></div>
    </div>
    <h3>Peers <small>({{ (x.mesh.peers ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'name' }, { key: 'address', mono: true }, { key: 'endpoint', mono: true, fmt: (r) => r.endpoint || 'NAT' }, { key: 'public_key', mono: true }, { key: 'heartbeat_at', fmt: (r) => ago(r.heartbeat_at), dim: true }]" :rows="x.mesh.peers ?? []" />
    <h3>wg-quick config</h3><pre class="json">{{ cfgText || '—' }}</pre>
  </div>

  <div class="card" v-if="x.geoip?.enabled">
    <h2>GeoIP</h2>
    <div class="tools"><button class="btn" @click="act(() => post('/v1/server/geoip/update'), 'update started')">Update now</button><button class="btn" @click="act(() => post('/v1/server/geoip/reload'), 'reloaded')">Reload file</button><input class="in" v-model="ip" placeholder="lookup ip" @keyup.enter="lookup" /><button class="btn" :disabled="!ip" @click="lookup">Lookup</button></div>
    <div class="grid">
      <div><Kv k="Edition" :v="x.geoip.edition" /><Kv k="Path" :v="x.geoip.path" mono /><Kv k="Database type" :v="x.geoip.database_type" /></div>
      <div><Kv k="Build age" :v="x.geoip.build_age_days != null ? x.geoip.build_age_days + ' days' : '—'" /><Kv k="Stale after / update every" :v="`${x.geoip.stale_days} / ${x.geoip.update_days} days`" /><Kv k="Update due" :v="x.geoip.update_due" /></div>
      <div><Kv k="Lookups / hits" :v="`${x.geoip.lookups} / ${x.geoip.hits}`" /><Kv k="Overrides" :v="x.geoip.overrides" /><Kv k="Last check / error" :v="`${ts(x.geoip.last_check)} / ${x.geoip.last_error ?? '—'}`" /></div>
    </div>
    <template v-if="x.lookup"><h3>Lookup</h3><JsonView :value="x.lookup" /></template>
  </div>

  <div class="card">
    <h2>Telemetry sinks <small>({{ (x.sinks?.sinks ?? []).length }})</small></h2>
    <div class="tools"><button class="btn" @click="act(() => post('/v1/server/analytics/sinks/flush'), 'flushed')">Flush</button><button class="btn" @click="ask('Upload partial batches now?') && act(() => post('/v1/server/analytics/sinks/flush?force=true'), 'force flushed')">Force flush</button><span class="dim">flush every {{ x.sinks?.flush_secs }}s · ingest {{ x.sinks?.ingest_url || 'local' }} · {{ x.sinks?.telemetry_only ? 'dedicated telemetry node' : 'shared with API' }}</span></div>
    <DataTable :cols="[{ key: 'name' }, { key: 'kind' }, { key: 'buffered' }, { key: 'uploads' }, { key: 'last_key', mono: true, dim: true }, { key: 'last_error', dim: true }, { key: 'last_flush_at', fmt: (r) => ts(r.last_flush_at), dim: true }]" :rows="x.sinks?.sinks ?? []" empty="No sinks configured ([analytics].sinks)" />
  </div>

  <div class="card" v-if="x.chunking && !x.chunking.error">
    <h2>Build chunk store</h2>
    <div class="tools"><button class="btn" @click="ask('Collect orphan chunks and stale uploads?') && act(() => post('/v1/server/builds/gc'), 'gc done')">Run GC</button></div>
    <div class="grid">
      <div><Kv k="Artifacts" :v="`${x.chunking.artifacts?.chunked} chunked of ${x.chunking.artifacts?.total}`" /><Kv k="Logical size" :v="mb(x.chunking.artifacts?.logical_bytes)" /><Kv k="Dedup ratio" :v="x.chunking.artifacts?.dedup_ratio + '×'" /></div>
      <div><Kv k="Store" :v="`${x.chunking.store?.backend} · ${x.chunking.store?.chunks} chunks · ${mb(x.chunking.store?.bytes)}`" /><Kv k="Orphans" :v="x.chunking.store?.orphans" /><Kv k="Pending uploads" :v="x.chunking.pending_uploads" /></div>
      <div><Kv k="Node cache" :v="x.chunking.cache?.enabled ? `${x.chunking.cache.files} files · ${mb(x.chunking.cache.bytes)} of ${mb(x.chunking.cache.max_bytes)}` : 'disabled'" /><Kv k="Cache dir" :v="x.chunking.cache?.dir" mono /><Kv k="Chunk min / avg / max" :v="`${mb(x.chunking.params?.min)} / ${mb(x.chunking.params?.avg)} / ${mb(x.chunking.params?.max)}`" /></div>
    </div>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
