<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id))
const F = computed(() => `/v1/server/fleets/${id.value}`)
const fleet = ref<any>(null); const err = ref(''); const x = ref<Record<string, any>>({})
const tab = ref(String(route.query.tab ?? 'overview'))
const tabs: [string, string][] = [['overview', 'overview'], ['servers', 'servers'], ['autoscaler', 'autoscaler'], ['rollout', 'rollout & builds'], ['health', 'health & alerts'], ['cost', 'cost & usage'], ['events', 'events & metrics'], ['spec', 'spec']]
const dlg = ref<'' | 'scale' | 'allocate' | 'build' | 'rule' | 'spec'>(''); const form = ref<any>({})
async function load() { err.value = ''; try { fleet.value = await get(F.value) } catch (e: any) { err.value = e.message } }
async function fx(k: string, p: string, force = false) { if (!force && x.value[k] !== undefined) return; try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = e.status === 404 ? null : { error: e.message } } }
function loadTab(t: string, force = false) {
  const f = F.value
  if (t === 'overview') { fx('counters', `${f}/counters`, force); fx('demand', `${f}/demand`, force); fx('reputation', `${f}/reputation`, force); fx('installs', `${f}/installs`, force) }
  if (t === 'servers') fx('servers', `${f}/servers`, force)
  if (t === 'autoscaler') { fx('autoscaler', `${f}/autoscaler`, force); fx('demand', `${f}/demand`, force) }
  if (t === 'rollout') { fx('rollout', `${f}/rollout`, force); fx('rollouts', `${f}/rollouts`, force); fx('builds', `${f}/builds`, force) }
  if (t === 'health') { fx('health', `${f}/health`, force); fx('alerts', `${f}/alerts`, force); fx('rules', `${f}/alerts/rules`, force) }
  if (t === 'cost') { fx('cost', `${f}/cost`, force); fx('usage', `${f}/usage`, force); fx('egress', `${f}/egress`, force) }
  if (t === 'events') { fx('events', `${f}/events?limit=200`, force); fx('metrics', `${f}/metrics`, force) }
}
watch(tab, (t) => { router.replace({ query: { ...route.query, tab: t } }); loadTab(t) }, { immediate: true })
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; await load(); loadTab(tab.value, true) } catch (e: any) { toast.err(e) } }
const ask = (m: string) => window.confirm(m)
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
const list = (k: string, sub?: string) => { const v = x.value[k]; if (!v || v.error) return []; const a = sub ? v[sub] : Array.isArray(v) ? v : Object.values(v).find(Array.isArray); return Array.isArray(a) ? a : [] }
function openDlg(k: typeof dlg.value) {
  dlg.value = k
  form.value = k === 'scale' ? { min_servers: fleet.value?.min_servers ?? 0, max_servers: fleet.value?.max_servers ?? 1, buffer: fleet.value?.buffer ?? 0 }
    : k === 'allocate' ? { build_version: '', wait_secs: 0, payload: '{}' }
    : k === 'build' ? { version: '', canary_percent: 10, canary_regions: '', notes: '', spec: JSON.stringify(fleet.value?.spec ?? {}, null, 2) }
    : k === 'rule' ? { name: '', metric: 'cpu_pct_avg', op: 'gt', threshold: 90, for_secs: 300, severity: 'warning' }
    : JSON.stringify(fleet.value?.spec ?? {}, null, 2)
}
const pct = (a: number, b: number) => (b ? Math.round((a / b) * 100) : 0)
</script>
<template>
  <div class="crumb"><router-link to="/servers">Fleets</router-link> › <span>{{ fleet?.name || id }}</span><span class="id">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px">
      <button class="btn s" @click="openDlg('scale')">Scale</button>
      <button class="btn s" @click="openDlg('allocate')">Allocate</button>
      <button class="btn s" @click="act(() => post(`${F}/reconcile`), 'reconcile requested')">Reconcile</button>
      <button class="btn s" @click="openDlg('spec')">Edit spec</button>
      <button class="btn s d" @click="ask('Delete this fleet and terminate its servers?') && act(async () => { await api('DELETE', F); router.push('/servers') }, 'fleet deleted')">Delete</button>
    </span>
  </div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="fleet">
    <div class="tabs"><a v-for="[t, l] in tabs" :key="t" :class="{ on: tab === t }" @click="tab = t">{{ l }}</a></div>

    <template v-if="tab === 'overview'">
      <div class="grid">
        <div><Kv k="Fleet ID" :v="fleet.id" mono copy /><Kv k="Name" :v="fleet.name" /><Kv k="Provider" :v="fleet.provider" /><Kv k="Region / pool" :v="`${fleet.region || '—'} / ${fleet.pool || '—'}`" /></div>
        <div><Kv k="Artifact" :v="fleet.spec?.artifact" mono /><Kv k="Image" :v="fleet.spec?.image" mono /><Kv k="Servers min / max / buffer" :v="`${fleet.min_servers} / ${fleet.max_servers} / ${fleet.buffer}`" /><Kv k="Policy" :v="fleet.policy" /></div>
        <div><Kv k="Ports" :v="`${fleet.port_start}–${fleet.port_end}`" mono /><Kv k="Created" :v="ts(fleet.created_at)" /><Kv k="Target ready" :v="x.demand?.target_ready" /><Kv k="Demand / hour" :v="x.demand?.per_hour" /></div>
      </div>
      <h3>Reputation</h3>
      <div class="grid" v-if="x.reputation"><div><Kv k="Score" :v="x.reputation.score ?? '—'" /></div><div><Kv k="Players served / rounds" :v="`${x.reputation.gameplay?.players_served ?? 0} / ${x.reputation.gameplay?.rounds ?? 0}`" /></div><div><Kv k="Crashes / kicks" :v="`${x.reputation.gameplay?.crashes ?? 0} / ${x.reputation.gameplay?.kicks ?? 0}`" /></div></div>
      <h3>Counters & lists</h3><JsonView :value="{ counters: x.counters?.counters ?? {}, lists: x.counters?.lists ?? {}, counter_target: x.counters?.counter_target }" />
      <h3>Install script <small v-if="x.installs">({{ x.installs.total }} runs, {{ x.installs.failed }} failed)</small> <button class="btn s" style="margin-left:8px" v-if="x.installs?.total" @click="act(() => post(`${F}/installs/reset`), 'install state reset')">Reset</button></h3>
      <DataTable :cols="[{ key: 'host_id', mono: true }, { key: 'status' }, { key: 'started_at', fmt: (r) => ts(r.started_at), dim: true }, { key: 'finished_at', fmt: (r) => ts(r.finished_at), dim: true }, { key: 'error', dim: true }]" :rows="list('installs', 'runs')" />
    </template>

    <template v-else-if="tab === 'servers'">
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'state' }, { key: 'region' }, { key: 'address', mono: true, fmt: (r) => r.address ? `${r.address}:${r.port ?? ''}` : '—' }, { key: 'players', fmt: (r) => `${r.players ?? 0} / ${r.capacity ?? '—'}` }, { key: 'build_version', label: 'build' }, { key: 'host_id', label: 'host', mono: true }, { key: 'age_secs', label: 'age', fmt: (r) => r.age_secs != null ? Math.round(r.age_secs / 60) + ' min' : '—', dim: true }, { key: 'last_health', label: 'health', dim: true }]" :rows="list('servers', 'servers')" pick @pick="(r) => router.push(`/servers/servers/${r.id}`)">
        <template #actions="{ row }"><span style="display:flex;gap:6px;justify-content:flex-end">
          <button class="btn s" v-if="!row.reserved_until" @click="act(() => post(`/v1/server/gameservers/${row.id}/reserve`, { secs: 600 }), 'reserved 10 min')">Reserve</button>
          <button class="btn s" v-else @click="act(() => post(`/v1/server/gameservers/${row.id}/unreserve`), 'unreserved')">Unreserve</button>
          <button class="btn s" @click="act(() => post(`${F}/servers/${row.id}/logs/archive`), 'log snapshot requested')">Archive logs</button>
          <button class="btn s d" @click="ask('Terminate this game server?') && act(() => api('DELETE', `/v1/server/gameservers/${row.id}`), 'terminated')">Terminate</button>
        </span></template>
      </DataTable>
    </template>

    <template v-else-if="tab === 'autoscaler'">
      <div class="tools"><button class="btn" v-if="!x.autoscaler?.suspended" @click="act(() => post(`${F}/autoscaler/suspend`), 'autoscaler suspended')">Suspend</button><button class="btn p" v-else @click="act(() => post(`${F}/autoscaler/resume`), 'autoscaler resumed')">Resume</button><span class="dim" v-if="x.autoscaler?.suspended">suspended</span></div>
      <div class="grid" v-if="x.autoscaler?.last">
        <div><Kv k="Last decision" :v="ts(x.autoscaler.last.at)" /><Kv k="Able to scale" :v="x.autoscaler.last.able_to_scale" /><Kv k="Applied" :v="`start ${x.autoscaler.last.applied?.start ?? 0} · stop ${x.autoscaler.last.applied?.stop ?? 0}`" /></div>
        <div><Kv k="Live / in game" :v="`${x.autoscaler.last.gauges?.live ?? 0} / ${x.autoscaler.last.gauges?.in_game ?? 0}`" /><Kv k="Allocated / capacity" :v="`${x.autoscaler.last.gauges?.allocated ?? 0} / ${x.autoscaler.last.gauges?.capacity ?? 0}`" /><Kv k="Players" :v="x.autoscaler.last.gauges?.players" /></div>
        <div><Kv k="CPU avg / max" :v="`${x.autoscaler.last.gauges?.cpu_pct_avg ?? '—'} / ${x.autoscaler.last.gauges?.cpu_pct_max ?? '—'}`" /><Kv k="Base + buffer" :v="`${x.autoscaler.last.base ?? 0} + ${x.autoscaler.last.buffer ?? 0}`" /><Kv k="Available" :v="x.autoscaler.last.available" /></div>
      </div>
      <h3>Decision chain</h3><JsonView :value="x.autoscaler?.last?.chain ?? []" />
      <h3>Demand forecast</h3>
      <DataTable :cols="[{ key: 'slot' }, { key: 'expected' }, { key: 'observed' }]" :rows="list('demand', 'slots')" />
      <h3>Raw</h3><JsonView :value="x.autoscaler ?? {}" />
    </template>

    <template v-else-if="tab === 'rollout'">
      <div class="grid" v-if="x.rollout">
        <div><Kv k="Strategy" :v="x.rollout.policy?.strategy" /><Kv k="Surge / unavailable" :v="`${x.rollout.policy?.max_surge} / ${x.rollout.policy?.max_unavailable}`" /><Kv k="Auto rollback" :v="x.rollout.policy?.auto_rollback" /></div>
        <div><Kv k="Progress"><div class="bar" style="width:140px"><i :style="{ width: (x.rollout.progress_pct ?? 0) + '%' }"></i></div><span>{{ x.rollout.progress_pct ?? 0 }}%</span></Kv><Kv k="Updated / outdated / live" :v="`${x.rollout.updated} / ${x.rollout.outdated} / ${x.rollout.live}`" /></div>
        <div><Kv k="Active rollout" :v="x.rollout.rollout?.state ?? 'none'" /><Kv k="Target version" :v="x.rollout.rollout?.version" mono /></div>
      </div>
      <div class="tools" v-if="x.rollout?.rollout">
        <button class="btn" v-if="x.rollout.rollout.state !== 'paused'" @click="act(() => post(`${F}/rollout/pause`), 'rollout paused')">Pause</button>
        <button class="btn p" v-else @click="act(() => post(`${F}/rollout/resume`), 'rollout resumed')">Resume</button>
        <button class="btn d" @click="ask('Roll back to the previous version?') && act(() => post(`${F}/rollout/rollback`), 'rolling back')">Rollback</button>
      </div>
      <h3>Builds <small>({{ list('builds', 'builds').length }})</small> <button class="btn s p" style="margin-left:8px" @click="openDlg('build')">Add build / canary</button></h3>
      <DataTable :cols="[{ key: 'version', mono: true }, { key: 'state' }, { key: 'canary_percent', label: 'canary %' }, { key: 'servers' }, { key: 'notes', dim: true }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="list('builds', 'builds')">
        <template #actions="{ row }"><span style="display:flex;gap:6px;justify-content:flex-end">
          <button class="btn s" v-if="row.state !== 'live'" @click="ask(`Promote ${row.version} to the whole fleet?`) && act(() => post(`${F}/builds/${encodeURIComponent(row.version)}/promote`), 'promoted')">Promote</button>
          <button class="btn s d" v-if="row.state !== 'live'" @click="ask(`Remove ${row.version}?`) && act(() => api('DELETE', `${F}/builds/${encodeURIComponent(row.version)}`), 'removed')">Remove</button>
        </span></template>
      </DataTable>
      <h3>Rollout history</h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'from' }, { key: 'to' }, { key: 'state' }, { key: 'started_at', fmt: (r) => ts(r.started_at), dim: true }, { key: 'finished_at', fmt: (r) => ts(r.finished_at), dim: true }]" :rows="list('rollouts', 'rollouts')" />
    </template>

    <template v-else-if="tab === 'health'">
      <div class="grid" v-if="x.health">
        <div><Kv k="Probe" :v="x.health.policy?.disabled ? 'disabled' : `every ${x.health.policy?.period_secs}s after ${x.health.policy?.initial_delay_secs}s`" /><Kv k="Failure threshold" :v="x.health.policy?.failure_threshold" /></div>
        <div><Kv k="Eviction" :v="x.health.eviction" /><Kv k="Timeout" :v="x.health.timeout_secs + 's'" /></div>
        <div><Kv k="Counts"><span v-for="(n, k) in x.health.counts" :key="String(k)" style="margin-right:10px">{{ k }} <b>{{ n }}</b></span><span v-if="!Object.keys(x.health.counts ?? {}).length">—</span></Kv></div>
      </div>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'status' }, { key: 'failures' }, { key: 'last_probe_at', fmt: (r) => ts(r.last_probe_at), dim: true }, { key: 'message', dim: true }]" :rows="list('health', 'servers')" />
      <h3>Firing alerts <small>({{ list('alerts', 'alerts').length }})</small> <button class="btn s" style="margin-left:8px" @click="act(() => post(`${F}/alerts/sweep`), 'alerts evaluated')">Evaluate now</button></h3>
      <DataTable :cols="[{ key: 'rule' }, { key: 'severity' }, { key: 'value' }, { key: 'since', fmt: (r) => ts(r.since ?? r.started_at), dim: true }, { key: 'message', dim: true }]" :rows="list('alerts', 'alerts')" />
      <h3>Alert rules <small>({{ list('rules', 'rules').length }})</small> <button class="btn s p" style="margin-left:8px" @click="openDlg('rule')">Add rule</button></h3>
      <DataTable :cols="[{ key: 'name' }, { key: 'metric' }, { key: 'op' }, { key: 'threshold' }, { key: 'for_secs', label: 'for (s)' }, { key: 'severity' }]" :rows="list('rules', 'rules')">
        <template #actions="{ row }"><button class="btn s d" @click="act(() => api('DELETE', `${F}/alerts/rules/${encodeURIComponent(row.name)}`), 'rule deleted')">Delete</button></template>
      </DataTable>
    </template>

    <template v-else-if="tab === 'cost'">
      <div class="grid" v-if="x.cost"><div><Kv k="Cost model" :v="x.cost.configured ? 'configured' : 'not configured (spec.cost)'" /></div><div><Kv k="Live servers" :v="x.cost.live" /></div><div><Kv k="Hourly estimate" :v="x.cost.hourly ?? x.cost.per_hour" /></div></div>
      <h3>Usage <small v-if="x.usage">({{ ts(x.usage.from) }} → {{ ts(x.usage.to) }}, {{ Math.max(0, x.usage.vcpu_hours ?? 0).toFixed(1) }} vCPU·h)</small></h3>
      <DataTable :cols="[{ key: 'day', fmt: (r) => r.day ?? r.date ?? ts(r.at) }, { key: 'servers' }, { key: 'server_hours', label: 'server·h' }, { key: 'vcpu_hours', label: 'vCPU·h' }, { key: 'cost' }]" :rows="list('usage', 'rows')" />
      <h3>Egress</h3><JsonView :value="x.egress ?? {}" />
    </template>

    <template v-else-if="tab === 'events'">
      <div class="grid" v-if="x.metrics">
        <div><Kv k="Live servers / players" :v="`${x.metrics.live_servers ?? 0} / ${x.metrics.players ?? 0}`" /><Kv k="Reporting" :v="`${x.metrics.aggregate?.servers_reporting ?? 0} servers, ${x.metrics.aggregate?.samples ?? 0} samples`" /></div>
        <div><Kv k="CPU avg / max %" :v="`${x.metrics.aggregate?.cpu_pct_avg ?? '—'} / ${x.metrics.aggregate?.cpu_pct_max ?? '—'}`" /><Kv k="Memory avg / max" :v="`${x.metrics.aggregate?.mem_bytes_avg ?? '—'} / ${x.metrics.aggregate?.mem_bytes_max ?? '—'}`" /></div>
        <div><Kv k="Tick avg / p99 max (ms)" :v="`${x.metrics.aggregate?.tick_ms_avg ?? '—'} / ${x.metrics.aggregate?.tick_p99_ms_max ?? '—'}`" /><Kv k="Firing alerts" :v="x.metrics.firing_alerts" /></div>
      </div>
      <h3>Per-server metrics</h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'cpu_pct' }, { key: 'mem_bytes' }, { key: 'tick_ms' }, { key: 'tick_p99_ms' }, { key: 'players' }, { key: 'at', fmt: (r) => ts(r.at), dim: true }]" :rows="list('metrics', 'servers')" />
      <h3>Events <small>({{ list('events', 'events').length }})</small></h3>
      <table><thead><tr><th style="width:170px">At</th><th style="width:220px">Kind</th><th>Payload</th></tr></thead><tbody>
        <tr v-for="(e, i) in list('events', 'events')" :key="i"><td class="dim">{{ ts(e.at ?? e.created_at) }}</td><td class="m">{{ e.kind }}</td><td class="m dim" style="word-break:break-all">{{ JSON.stringify(e.payload ?? e.data ?? {}) }}</td></tr>
        <tr v-if="!list('events', 'events').length"><td colspan="3" class="empty">No Data</td></tr></tbody></table>
    </template>

    <template v-else-if="tab === 'spec'"><JsonView :value="fleet.spec" /></template>
  </div>

  <Drawer v-if="dlg === 'scale'" title="Scale fleet" @close="dlg = ''"><div class="form">
    <div class="f"><label>Min servers</label><input class="in" type="number" v-model.number="form.min_servers" /></div>
    <div class="f"><label>Max servers</label><input class="in" type="number" v-model.number="form.max_servers" /></div>
    <div class="f"><label>Ready buffer</label><input class="in" type="number" v-model.number="form.buffer" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${F}/scale`, form), 'scaled')">Apply</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'allocate'" title="Allocate a server" @close="dlg = ''"><div class="form">
    <div class="f"><label>Build version (optional selector)</label><input class="in" v-model="form.build_version" /></div>
    <div class="f"><label>Wait for a ready server, seconds (0 = fail fast)</label><input class="in" type="number" v-model.number="form.wait_secs" /></div>
    <div class="f"><label>Payload handed to the server (JSON)</label><textarea class="in" v-model="form.payload" style="min-height:100px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(async () => { const r = await post(`${F}/allocate`, { payload: JSON.parse(form.payload || '{}'), wait_secs: form.wait_secs, selector: form.build_version ? { build_version: form.build_version } : {} }); toast.show(`allocated ${r.server?.id ?? r.id ?? ''}`) }, 'allocated')">Allocate</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'build'" title="Add build (canary)" @close="dlg = ''"><div class="form">
    <div class="f"><label>Version</label><input class="in" v-model="form.version" placeholder="1.2.0" /></div>
    <div class="f"><label>Canary share, % (0 = active at once)</label><input class="in" type="number" v-model.number="form.canary_percent" /></div>
    <div class="f"><label>Canary regions (comma-separated, empty = all)</label><input class="in" v-model="form.canary_regions" /></div>
    <div class="f"><label>Notes</label><input class="in" v-model="form.notes" /></div>
    <div class="f"><label>Spec (JSON — image / artifact / command / env / vcpu)</label><textarea class="in" v-model="form.spec" style="min-height:220px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.version" @click="act(() => post(`${F}/builds`, { version: form.version, spec: JSON.parse(form.spec), canary_percent: form.canary_percent, canary_regions: form.canary_regions.split(',').map((r: string) => r.trim()).filter(Boolean), notes: form.notes }), 'build added')">Add</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'rule'" title="Alert rule" @close="dlg = ''"><div class="form">
    <div class="f"><label>Name</label><input class="in" v-model="form.name" /></div>
    <div class="f"><label>Metric</label><select class="sel" v-model="form.metric"><option>cpu_pct_avg</option><option>cpu_pct_max</option><option>mem_pct_avg</option><option>tick_p99_ms_max</option><option>players</option><option>live</option><option>unhealthy</option><option>crashes</option></select></div>
    <div class="f"><label>Operator</label><select class="sel" v-model="form.op"><option value="gt">&gt;</option><option value="gte">&gt;=</option><option value="lt">&lt;</option><option value="lte">&lt;=</option></select></div>
    <div class="f"><label>Threshold</label><input class="in" type="number" v-model.number="form.threshold" /></div>
    <div class="f"><label>For, seconds</label><input class="in" type="number" v-model.number="form.for_secs" /></div>
    <div class="f"><label>Severity</label><select class="sel" v-model="form.severity"><option>info</option><option>warning</option><option>critical</option></select></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.name" @click="act(() => { const { name, ...b } = form; return api('PUT', `${F}/alerts/rules/${encodeURIComponent(name)}`, b) }, 'rule saved')">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'spec'" title="Fleet spec" @close="dlg = ''">
    <textarea class="in" v-model="form" style="min-height:420px"></textarea>
    <div class="form"><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => api('PUT', F, { spec: JSON.parse(form) }), 'spec saved')">Save</button></div></div>
  </Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
