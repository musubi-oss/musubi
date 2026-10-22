<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const queues = ref<any[]>([]); const stats = ref<any>(null); const mmf = ref<any>(null); const err = ref(''); const dlg = ref(''); const q = ref<any>(null); const sel = ref<any>(null); const sim = ref<any>(null); const simForm = ref({ tickets: 100 }); const raw = ref(false)
interface AR { kind: string; property: string; max: number; step: number; every_secs: number; max_limit: number; min: number; missing: string }
const ar = ref<AR[]>([]); const roles = ref<{ name: string; count: number }[]>([]); const crossplay = ref(''); const ext = ref('{}'); const mmfHeaders = ref('')
async function load() {
  err.value = ''
  const r = await Promise.allSettled([get('/v1/server/matchmaker/queues'), get('/v1/server/matchmaker/stats'), get('/v1/server/matchmaker/mmf')])
  queues.value = r[0].status === 'fulfilled' ? r[0].value.queues ?? [] : (err.value = (r[0] as any).reason.message, []); stats.value = r[1].status === 'fulfilled' ? r[1].value : null; mmf.value = r[2].status === 'fulfilled' ? r[2].value : null
}
onMounted(load)
function blank() { return { name: '', min_count: 2, max_count: 2, teams: null, mmr: null, rules: [], sort: null, ping: null, roles: {}, no_repeat_secs: 0, crossplay: [], priority_after_secs: 0, ready_check_secs: null, dodge_penalty_secs: null, mmf: null, extensions: {} } }
function edit(x: any) {
  q.value = JSON.parse(JSON.stringify(x ?? blank())); q.value.teams ||= null; q.value.mmr ||= null; q.value.sort ||= null; q.value.ping ||= null; q.value.mmf ||= null
  ar.value = (q.value.rules ?? []).map((r: any) => ({ kind: r.kind ?? 'difference', property: r.property ?? '', max: r.max ?? 0, step: r.step ?? 0, every_secs: r.every_secs ?? 0, max_limit: r.max_limit ?? 0, min: r.min ?? 0, missing: r.missing ?? 'reject' }))
  roles.value = Object.entries(q.value.roles ?? {}).map(([name, count]) => ({ name, count: Number(count) })); crossplay.value = (q.value.crossplay ?? []).map((g: string[]) => g.join('+')).join(', '); ext.value = JSON.stringify(q.value.extensions ?? {}, null, 2); mmfHeaders.value = JSON.stringify(q.value.mmf?.headers ?? {})
  dlg.value = 'edit'
}
const toggle = (k: 'teams' | 'mmr' | 'sort' | 'ping' | 'mmf', d: any) => { q.value[k] = q.value[k] ? null : d }
function body() {
  const b = { ...q.value, rules: ar.value.map((r) => ({ ...r, max: Number(r.max), step: Number(r.step), every_secs: Number(r.every_secs), max_limit: Number(r.max_limit), min: Number(r.min) })), roles: Object.fromEntries(roles.value.filter((r) => r.name).map((r) => [r.name, Number(r.count)])), crossplay: crossplay.value.split(',').map((g) => g.trim()).filter(Boolean).map((g) => g.split('+').map((s) => s.trim()).filter(Boolean)), extensions: JSON.parse(ext.value || '{}'), min_count: Number(q.value.min_count), max_count: Number(q.value.max_count), no_repeat_secs: Number(q.value.no_repeat_secs || 0), priority_after_secs: Number(q.value.priority_after_secs || 0), ready_check_secs: q.value.ready_check_secs || null, dodge_penalty_secs: q.value.dodge_penalty_secs || null }
  if (b.mmf) b.mmf = { ...b.mmf, headers: JSON.parse(mmfHeaders.value || '{}') }
  delete b.paused; delete b.draining; delete b.status_reason
  return b
}
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; load(); return r } catch (x: any) { toast.err(x) } }
const waiting = (n: string) => stats.value?.waiting?.[n] ?? 0
const age = (n: string) => { const a = stats.value?.waiting_ages?.[n]; return a == null ? '—' : typeof a === 'object' ? `${a.p50 ?? a.avg ?? '?'}s / max ${a.max ?? '?'}s` : `${a}s` }
const state = (x: any) => x.draining ? 'draining' : x.paused ? 'paused' : 'open'
const summary = (x: any) => [x.teams ? `${x.teams.count}×${x.teams.size} teams` : `${x.min_count}–${x.max_count} players`, x.mmr ? `mmr ±${x.mmr.initial_delta}→${x.mmr.max_delta}` : null, (x.rules ?? []).length ? `${x.rules.length} rule(s)` : null, x.ping ? `ping ≤${x.ping.initial_ms}→${x.ping.max_ms}ms` : null, x.mmf ? `mmf ${x.mmf.kind}` : null].filter(Boolean).join(' · ')
const pg2 = usePager(() => queues)
const pg1 = usePager(() => ar)
</script>
<template>
  <div class="crumb"><span>Matchmaking</span><span class="id" v-if="stats">{{ queues.length }} queues · {{ Object.values(stats.waiting ?? {}).reduce((a: number, b: any) => a + Number(b), 0) }} waiting · {{ stats.open_matches ?? 0 }} open matches</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="raw = !raw">{{ raw ? 'Hide raw' : 'Raw' }}</button><button class="btn s" @click="dlg = 'sim'; sim = null; sel = queues[0]">Simulate</button><button class="btn s p" @click="edit(null)">New queue</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">A ticket enters one queue; the matcher widens MMR / attribute / ping windows over time, then forms teams. Paused queues accept tickets but do not match; draining queues cancel waiting tickets.</p>
    <table><thead><tr><th>Queue</th><th style="width:90px">State</th><th>Composition</th><th style="width:100px">Waiting</th><th style="width:150px">Wait p50 / max</th><th style="width:260px"></th></tr></thead><tbody>
      <tr v-for="x in pg2.rows" :key="x.name"><td class="m row" @click="edit(x)">{{ x.name }}</td><td :class="{ bad: x.draining, dim: !x.paused && !x.draining }">{{ state(x) }}<span class="dim" v-if="x.status_reason"> — {{ x.status_reason }}</span></td><td class="dim">{{ summary(x) }}</td><td>{{ waiting(x.name) }}</td><td class="dim">{{ age(x.name) }}</td>
        <td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" v-if="!x.paused" @click="act(() => post(`/v1/server/matchmaker/queues/${encodeURIComponent(x.name)}/pause`), 'paused')">Pause</button><button class="btn s" v-else @click="act(() => post(`/v1/server/matchmaker/queues/${encodeURIComponent(x.name)}/resume`), 'resumed')">Resume</button> <button class="btn s" @click="ask(`Drain ${x.name}: cancel every waiting ticket?`) && act(() => post(`/v1/server/matchmaker/queues/${encodeURIComponent(x.name)}/drain`), 'draining')">Drain</button> <button class="btn s" @click="ask(`Delete queue ${x.name}?`) && act(() => api('DELETE', `/v1/server/matchmaker/queues/${encodeURIComponent(x.name)}`), 'deleted')">×</button></td></tr>
      <tr v-if="!pg2.total"><td colspan="6" class="empty">No queues — create one; clients reference it by name</td></tr></tbody></table><Pager :p="pg2" />
    <template v-if="mmf?.queues?.length"><h3>External match functions</h3><JsonView :value="mmf" /></template>
    <template v-if="raw"><h3>Stats</h3><JsonView :value="stats ?? {}" /></template>
  </div>
  <Drawer v-if="dlg === 'edit'" :title="q.name ? `Queue ${q.name}` : 'New queue'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Name</label><input class="in m" v-model="q.name" :disabled="!!queues.find((x) => x.name === q.name && q.name)" /></div>
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Min players</label><input class="in" type="number" min="1" v-model.number="q.min_count" /></div><div style="flex:1"><label>Max players</label><input class="in" type="number" min="1" v-model.number="q.max_count" /></div></div>
    <h3><label><input type="checkbox" :checked="!!q.teams" @change="toggle('teams', { count: 2, size: 1, kind: 'balanced', property: 'mmr', order: 'desc' })" /> Teams</label></h3>
    <div class="f" v-if="q.teams" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>Count</label><input class="in" type="number" min="1" v-model.number="q.teams.count" style="width:90px" /></div><div><label>Size</label><input class="in" type="number" min="1" v-model.number="q.teams.size" style="width:90px" /></div><div><label>Fill</label><select class="sel" v-model="q.teams.kind"><option value="balanced">balanced — snake draft by property</option><option value="random">random</option><option value="fixed">fixed — keep party order</option></select></div><div><label>Property / order</label><div style="display:flex;gap:6px"><input class="in m" v-model="q.teams.property" style="width:110px" /><select class="sel" v-model="q.teams.order"><option>desc</option><option>asc</option></select></div></div></div>
    <h3><label><input type="checkbox" :checked="!!q.mmr" @change="toggle('mmr', { property: 'mmr', initial_delta: 100, step: 50, every_secs: 10, max_delta: 500 })" /> Skill window</label></h3>
    <div class="f" v-if="q.mmr" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>Property</label><input class="in m" v-model="q.mmr.property" style="width:110px" /></div><div><label>Initial ±</label><input class="in" type="number" v-model.number="q.mmr.initial_delta" style="width:90px" /></div><div><label>+ step</label><input class="in" type="number" v-model.number="q.mmr.step" style="width:90px" /></div><div><label>every (s)</label><input class="in" type="number" v-model.number="q.mmr.every_secs" style="width:90px" /></div><div><label>max ±</label><input class="in" type="number" v-model.number="q.mmr.max_delta" style="width:90px" /></div></div>
    <h3>Attribute rules <small>({{ ar.length }})</small> <button class="btn s p" style="margin-left:8px" @click="ar.push({ kind: 'difference', property: '', max: 0, step: 0, every_secs: 0, max_limit: 0, min: 0, missing: 'reject' })">Add</button></h3>
    <table v-if="ar.length"><thead><tr><th>Kind</th><th>Property</th><th>Max</th><th>Step</th><th>Every (s)</th><th>Max limit</th><th>Min</th><th>Missing</th><th></th></tr></thead><tbody>
      <tr v-for="(r, i) in pg1.rows" :key="i"><td><input class="in" list="rule-kinds" v-model="r.kind" style="width:100px" /></td><td><input class="in m" v-model="r.property" style="width:110px" /></td><td><input class="in" type="number" v-model.number="r.max" style="width:70px" /></td><td><input class="in" type="number" v-model.number="r.step" style="width:70px" /></td><td><input class="in" type="number" v-model.number="r.every_secs" style="width:70px" /></td><td><input class="in" type="number" v-model.number="r.max_limit" style="width:80px" /></td><td><input class="in" type="number" v-model.number="r.min" style="width:60px" /></td><td><select class="sel" v-model="r.missing"><option>reject</option><option>accept</option></select></td><td><button class="btn s" @click="ar.splice(i, 1)">×</button></td></tr></tbody></table><Pager :p="pg1" />
    <datalist id="rule-kinds"><option value="difference" /><option value="equal" /><option value="count" /></datalist>
    <h3><label><input type="checkbox" :checked="!!q.ping" @change="toggle('ping', { prefix: 'ping_', initial_ms: 60, step_ms: 20, every_secs: 10, max_ms: 200 })" /> Ping window</label></h3>
    <div class="f" v-if="q.ping" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>Property prefix</label><input class="in m" v-model="q.ping.prefix" style="width:110px" /></div><div><label>Initial ms</label><input class="in" type="number" v-model.number="q.ping.initial_ms" style="width:90px" /></div><div><label>+ ms</label><input class="in" type="number" v-model.number="q.ping.step_ms" style="width:80px" /></div><div><label>every (s)</label><input class="in" type="number" v-model.number="q.ping.every_secs" style="width:80px" /></div><div><label>max ms</label><input class="in" type="number" v-model.number="q.ping.max_ms" style="width:90px" /></div></div>
    <h3><label><input type="checkbox" :checked="!!q.sort" @change="toggle('sort', { kind: 'property', property: 'mmr', order: 'desc' })" /> Sort tickets</label></h3>
    <div class="f" v-if="q.sort" style="display:flex;gap:8px"><input class="in" v-model="q.sort.kind" placeholder="kind" style="width:110px" /><input class="in m" v-model="q.sort.property" placeholder="property" style="width:140px" /><select class="sel" v-model="q.sort.order"><option>desc</option><option>asc</option></select></div>
    <h3>Roles <small>(required per match)</small> <button class="btn s p" style="margin-left:8px" @click="roles.push({ name: '', count: 1 })">Add</button></h3>
    <div class="f" v-for="(r, i) in roles" :key="i" style="display:flex;gap:8px"><input class="in m" v-model="r.name" placeholder="healer" style="width:160px" /><input class="in" type="number" min="1" v-model.number="r.count" style="width:80px" /><button class="btn s" @click="roles.splice(i, 1)">×</button></div>
    <div class="f"><label>Crossplay groups (platforms that may meet, "+" inside a group, "," between)</label><input class="in" v-model="crossplay" placeholder="pc+xbox+psn, ios+android" /></div>
    <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>No repeat opponents (s)</label><input class="in" type="number" min="0" v-model.number="q.no_repeat_secs" style="width:120px" /></div><div><label>Priority after (s)</label><input class="in" type="number" min="0" v-model.number="q.priority_after_secs" style="width:120px" /></div><div><label>Ready check (s)</label><input class="in" type="number" min="0" v-model.number="q.ready_check_secs" placeholder="off" style="width:120px" /></div><div><label>Dodge penalty (s)</label><input class="in" type="number" min="0" v-model.number="q.dodge_penalty_secs" placeholder="off" style="width:120px" /></div></div>
    <h3><label><input type="checkbox" :checked="!!q.mmf" @change="toggle('mmf', { kind: 'http', url: '', headers: {}, timeout_ms: 2000, every_ms: 1000, min_tickets: 2, fallback_after_secs: 0, propose: '', score: '' })" /> External match function</label></h3>
    <template v-if="q.mmf"><div class="f" style="display:flex;gap:8px"><select class="sel" v-model="q.mmf.kind"><option value="http">http — POST tickets, get proposals</option><option value="wasm">wasm — runtime module</option></select><input class="in m" v-model="q.mmf.url" :placeholder="q.mmf.kind === 'http' ? 'https://mmf.example/propose' : 'module name'" style="flex:1" /></div>
      <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>Timeout ms</label><input class="in" type="number" v-model.number="q.mmf.timeout_ms" style="width:100px" /></div><div><label>Every ms</label><input class="in" type="number" v-model.number="q.mmf.every_ms" style="width:100px" /></div><div><label>Min tickets</label><input class="in" type="number" v-model.number="q.mmf.min_tickets" style="width:100px" /></div><div><label>Fallback to built-in after (s)</label><input class="in" type="number" v-model.number="q.mmf.fallback_after_secs" style="width:120px" /></div></div>
      <div class="f" style="display:flex;gap:8px"><input class="in m" v-model="q.mmf.propose" placeholder="propose fn (wasm)" /><input class="in m" v-model="q.mmf.score" placeholder="score fn (wasm)" /></div><div class="f"><label>Headers (JSON)</label><input class="in m" v-model="mmfHeaders" /></div>
      <div class="row" v-if="q.name"><button class="btn s" @click="act(() => post(`/v1/server/matchmaker/queues/${encodeURIComponent(q.name)}/mmf/test`), 'mmf reachable')">Test match function</button></div></template>
    <div class="f"><label>Extensions (JSON, passed to game servers with the match)</label><textarea class="in m" v-model="ext" style="min-height:70px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!q.name" @click="act(() => put('/v1/server/matchmaker/queues', body()), 'queue saved')">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'sim'" title="Simulate matchmaking" @close="dlg = ''"><div class="form"><p class="note">Generates synthetic tickets against a queue's rules and reports match sizes and wait distribution — nothing is persisted.</p>
    <div class="f"><label>Queue</label><select class="sel" v-model="sel"><option v-for="x in queues" :key="x.name" :value="x">{{ x.name }}</option></select></div><div class="f"><label>Tickets</label><input class="in" type="number" min="1" v-model.number="simForm.tickets" /></div>
    <pre class="pre" v-if="sim" style="max-height:260px">{{ JSON.stringify(sim, null, 2) }}</pre>
    <div class="row"><button class="btn" @click="dlg = ''">Close</button><button class="btn p" :disabled="!sel" @click="async () => { try { sim = await post('/v1/server/matchmaker/simulate', { queue: sel.name, tickets: Number(simForm.tickets) }) } catch (x: any) { toast.err(x) } }">Run</button></div></div></Drawer>
</template>
