<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const runs = ref<any[]>([]); const active = ref<any>(null); const err = ref(''); const dlg = ref(''); const sel = ref<any>(null); const cmp = ref<any>(null); const base = ref(''); let timer: any
const f = ref({ name: 'smoke', scenario: 'ping', bots: 100, ramp_secs: 10, duration_secs: 60, rate_hz: 1, channel: 'global', match_size: 2, payload_bytes: 64, url: '', format: 'json' })
async function load() { err.value = ''; try { const r = await get('/v1/admin/dev/loadtests'); runs.value = r.runs ?? []; active.value = r.active ?? null } catch (x: any) { err.value = x.message } }
onMounted(() => { load(); timer = setInterval(() => { if (active.value) load() }, 3000) }); onUnmounted(() => clearInterval(timer))
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
async function compare() { try { cmp.value = await get(`/v1/admin/dev/loadtests/${sel.value.id}/compare/${base.value}`) } catch (x: any) { toast.err(x) } }
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
const pct = (r: any) => r.report?.errors?.rate_pct
const lat = (r: any, k: string) => r.report?.[k]?.p95 ?? r.report?.latency?.[k]?.p95
const start = () => act(() => post('/v1/admin/dev/loadtests', { ...f.value, url: f.value.url || null, bots: Number(f.value.bots), ramp_secs: Number(f.value.ramp_secs), duration_secs: Number(f.value.duration_secs), rate_hz: Number(f.value.rate_hz), match_size: Number(f.value.match_size), payload_bytes: Number(f.value.payload_bytes) }), 'load test started')
</script>
<template>
  <div class="crumb"><span>Load tests</span><span class="id">{{ runs.length }} runs{{ active ? ` · running: ${active.name ?? active.id}` : '' }}</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="active" @click="act(() => post(`/v1/admin/dev/loadtests/${active.id}/stop`), 'stopping')">Stop running</button><button class="btn s p" :disabled="!!active" @click="dlg = 'new'">New run</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Bots are spawned by this node against itself (or <i>url</i>): they log in, then run the scenario at <i>rate</i> — ping (realtime echo), chat (channel messages), match (matchmaking tickets → match), rpc (server RPC), custom (module hook). Reports keep connect / request latency percentiles and error rates; compare two runs to catch regressions in CI.</p>
    <table><thead><tr><th>Name</th><th style="width:80px">Scenario</th><th style="width:70px">Bots</th><th style="width:90px">Duration</th><th style="width:110px">Connect p95</th><th style="width:110px">Request p95</th><th style="width:90px">Errors</th><th style="width:140px">Started</th><th style="width:150px"></th></tr></thead><tbody>
      <tr v-for="r in runs" :key="r.id" :class="{ on: sel?.id === r.id }"><td class="row" @click="sel = r; cmp = null; dlg = 'report'">{{ r.name }}<div class="dim m" style="font-size:11px">{{ r.node }}</div></td><td class="dim">{{ r.scenario ?? r.report?.scenario ?? '—' }}</td><td>{{ r.report?.bots?.requested ?? r.bots ?? '—' }}<span class="dim" v-if="r.report?.bots?.failed"> ({{ r.report.bots.failed }} failed)</span></td><td class="dim">{{ r.report?.duration_secs ?? '…' }} s</td><td class="dim">{{ lat(r, 'connect') != null ? lat(r, 'connect') + ' ms' : '—' }}</td><td class="dim">{{ lat(r, 'request') != null ? lat(r, 'request') + ' ms' : '—' }}</td><td :class="{ bad: pct(r) > 1 }">{{ pct(r) != null ? pct(r) + '%' : (r.finished_at ? '0%' : 'running') }}</td><td class="dim">{{ ts(r.created_at) }}</td>
        <td><button class="btn s" @click="sel = r; cmp = null; dlg = 'report'">Report</button> <button class="btn s" v-if="!r.finished_at" @click="act(() => post(`/v1/admin/dev/loadtests/${r.id}/stop`), 'stopping')">Stop</button> <button class="btn s" @click="ask('Delete this run?') && act(() => api('DELETE', `/v1/admin/dev/loadtests/${r.id}`), 'deleted')">×</button></td></tr>
      <tr v-if="!runs.length"><td colspan="9" class="empty">No runs yet</td></tr></tbody></table>
  </div>
  <Drawer v-if="dlg === 'new'" title="Start a load test" @close="dlg = ''"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Name</label><input class="in" v-model="f.name" /></div><div><label>Scenario</label><select class="sel" v-model="f.scenario"><option value="ping">ping</option><option value="chat">chat</option><option value="match">match</option><option value="rpc">rpc</option><option value="custom">custom</option></select></div></div>
    <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>Bots</label><input class="in" type="number" min="1" v-model.number="f.bots" style="width:100px" /></div><div><label>Ramp (s)</label><input class="in" type="number" min="0" v-model.number="f.ramp_secs" style="width:100px" /></div><div><label>Duration (s)</label><input class="in" type="number" min="1" v-model.number="f.duration_secs" style="width:110px" /></div><div><label>Rate per bot (Hz)</label><input class="in" type="number" min="0.1" step="0.1" v-model.number="f.rate_hz" style="width:120px" /></div></div>
    <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div v-if="f.scenario === 'chat'"><label>Channel</label><input class="in m" v-model="f.channel" style="width:140px" /></div><div v-if="f.scenario === 'match'"><label>Match size</label><input class="in" type="number" min="2" v-model.number="f.match_size" style="width:100px" /></div><div><label>Payload bytes</label><input class="in" type="number" min="0" v-model.number="f.payload_bytes" style="width:110px" /></div></div>
    <div class="f"><label>Target URL (empty = this node)</label><input class="in m" v-model="f.url" placeholder="https://staging.example:7350" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!f.name" @click="start">Start</button></div></div></Drawer>
  <Drawer v-if="dlg === 'report'" :title="`Run ${sel.name}`" @close="dlg = ''"><div class="form">
    <div class="grid" v-if="sel.report"><div><Kv k="Bots" :v="`${sel.report.bots?.connected ?? 0} / ${sel.report.bots?.requested ?? 0} connected · ${sel.report.bots?.failed ?? 0} failed`" /><Kv k="Duration" :v="(sel.report.duration_secs ?? 0) + ' s'" /></div><div><Kv k="Connect ms" :v="sel.report.connect ? `p50 ${sel.report.connect.p50} · p95 ${sel.report.connect.p95} · p99 ${sel.report.connect.p99} · max ${sel.report.connect.max}` : '—'" /><Kv k="Request ms" :v="sel.report.request ? `p50 ${sel.report.request.p50} · p95 ${sel.report.request.p95} · p99 ${sel.report.request.p99}` : '—'" /></div><div><Kv k="Errors" :v="sel.report.errors ? `${sel.report.errors.rate_pct}% — ${Object.entries(sel.report.errors.by_kind ?? {}).map(([k, n]) => `${k} ${n}`).join(', ') || 'none'}` : '—'" /></div></div>
    <div class="f" style="display:flex;gap:8px;align-items:flex-end"><div style="flex:1"><label>Compare against run</label><select class="sel" v-model="base"><option v-for="r in runs.filter((r) => r.id !== sel.id && r.finished_at)" :key="r.id" :value="r.id">{{ r.name }} · {{ ts(r.created_at) }}</option></select></div><button class="btn s" :disabled="!base" @click="compare">Compare</button></div>
    <template v-if="cmp"><h3>Comparison</h3><JsonView :value="cmp" /></template>
    <h3>Full report</h3><JsonView :value="sel.report ?? sel" />
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
