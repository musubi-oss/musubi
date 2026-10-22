<script setup lang="ts">
// The KPI sheet the studio checks every morning: active users and stickiness, retention
// cohorts, revenue per day, top events, geography, a funnel of your choosing — all computed by
// the node from its own event store (no third-party analytics needed).
import { ref, onMounted } from 'vue'
import { get, post, put, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import Bars from '../../components/Bars.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const days = ref(30); const funnel = ref('login,match_start,match_end'); const samplePct = ref(100)
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = { error: e.message } } }
async function load() { err.value = ''; await Promise.all([fx('au', `/v1/server/analytics/kpi/active_users?days=${days.value}`), fx('ret', `/v1/server/analytics/kpi/retention?days=${days.value}`), fx('rev', `/v1/server/analytics/kpi/revenue?days=${days.value}`), fx('ev', `/v1/server/analytics/kpi/events?days=${days.value}`), fx('geo', '/v1/server/analytics/geo'), fx('attr', '/v1/server/analytics/attribution'), fx('cr', '/v1/server/crashes/summary'), fx('samp', '/v1/server/analytics/sampling'), fx('rep', '/v1/server/analytics/reports/latest')]); samplePct.value = x.value.samp?.global_pct ?? 100; runFunnel() }
async function runFunnel() { await fx('fun', '/v1/server/analytics/kpi/funnel' + qs({ steps: funnel.value, days: days.value })) }
onMounted(load)
const pct = (v?: number) => v == null ? '—' : (v * 100).toFixed(v >= 0.1 ? 0 : 1) + '%'
const revByDay = () => { const m = new Map<string, number>(); for (const d of x.value.rev?.daily ?? []) m.set(d.day, (m.get(d.day) ?? 0) + d.amount); return [...m.entries()].sort().map(([label, v]) => ({ label, v })) }
async function saveSampling() { try { await put('/v1/server/analytics/sampling', { global_pct: samplePct.value, always_enabled: x.value.samp?.always_enabled ?? [] }); toast.show('sampling saved'); load() } catch (e: any) { toast.err(e) } }
async function flush() { try { await post('/v1/server/analytics/flush'); toast.show('flushed'); load() } catch (e: any) { toast.err(e) } }
</script>
<template>
  <div class="crumb"><span>Overview</span><span style="margin-left:auto;display:flex;gap:8px;align-items:center"><select class="in" v-model.number="days" @change="load"><option :value="7">7 days</option><option :value="30">30 days</option><option :value="90">90 days</option></select><button class="btn s" @click="flush">Flush buffered events</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="stats">
    <div class="stat"><div class="k">DAU</div><div class="v">{{ x.au?.dau ?? '—' }}<small>stickiness {{ pct(x.au?.stickiness) }}</small></div></div>
    <div class="stat"><div class="k">WAU / MAU</div><div class="v">{{ x.au?.wau ?? '—' }}<small>/ {{ x.au?.mau ?? '—' }}</small></div></div>
    <div class="stat"><div class="k">Revenue ({{ days }} d)</div><div class="v">{{ (x.rev?.daily ?? []).reduce((a: number, d: any) => a + d.amount, 0) }}<small>{{ (x.rev?.daily ?? []).reduce((a: number, d: any) => a + d.purchases, 0) }} purchases · {{ [...new Set((x.rev?.daily ?? []).map((d: any) => d.currency))].length }} currencies</small></div></div>
    <div class="stat"><div class="k">D1 / D7 retention</div><div class="v">{{ pct(x.ret?.cohorts?.[0]?.d1) }}<small>/ {{ pct(x.ret?.cohorts?.[0]?.d7) }} · newest cohort</small></div></div>
    <div class="stat"><div class="k">Crash-free users (24 h)</div><div class="v" :class="{ warn: x.cr && x.cr.crash_free_users_pct < 99 }">{{ x.cr ? x.cr.crash_free_users_pct.toFixed(1) + '%' : '—' }}<small>{{ x.cr?.crashes ?? 0 }} crashes · {{ x.cr?.groups ?? 0 }} groups · <router-link to="/players/crashes">reports</router-link></small></div></div>
  </div>
  <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px">
    <div class="card"><h3 style="margin-top:0">Daily active users</h3><Bars :values="(x.au?.daily ?? []).map((d: any) => ({ label: d.day, v: d.users }))" /><div class="dim" style="font-size:12px;margin-top:6px">{{ (x.au?.daily ?? []).length }} days with activity · {{ (x.au?.daily ?? []).reduce((a: number, d: any) => a + d.events, 0) }} events</div></div>
    <div class="card"><h3 style="margin-top:0">Revenue per day</h3><Bars :values="revByDay()" /><div class="dim" style="font-size:12px;margin-top:6px">all currencies summed; the table below splits them</div></div>
  </div>
  <div class="card"><h3 style="margin-top:0">Retention cohorts <small>(share of the sign-up day's users back after N days)</small></h3>
    <DataTable :cols="[{ key: 'cohort' }, { key: 'size' }, { key: 'd1', fmt: (r) => pct(r.d1) }, { key: 'd3', fmt: (r) => pct(r.d3) }, { key: 'd7', fmt: (r) => pct(r.d7) }, { key: 'd14', fmt: (r) => pct(r.d14) }, { key: 'd30', fmt: (r) => pct(r.d30) }]" :rows="x.ret?.cohorts ?? []" empty="No cohorts yet — retention needs sign-ups and a few days of events" /></div>
  <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px">
    <div class="card"><h3 style="margin-top:0">Revenue by day and currency</h3><DataTable :cols="[{ key: 'day' }, { key: 'currency', mono: true }, { key: 'amount' }, { key: 'purchases' }, { key: 'buyers' }, { key: 'arpdau', label: 'ARPDAU', fmt: (r) => r.arpdau.toFixed(2) }, { key: 'arppu', label: 'ARPPU', fmt: (r) => r.arppu.toFixed(2) }]" :rows="x.rev?.daily ?? []" empty="No purchases in the window" /></div>
    <div class="card"><h3 style="margin-top:0">Top events</h3><DataTable :cols="[{ key: 'name', mono: true }, { key: 'count' }, { key: 'users' }, { key: 'per_user', label: 'per user', fmt: (r) => (r.count / Math.max(1, r.users)).toFixed(1) }]" :rows="(x.ev?.events ?? []).slice(0, 25)" empty="No events in the window" /><router-link to="/analytics/events" class="dim" style="font-size:12px">explore events →</router-link></div>
  </div>
  <div class="card"><h3 style="margin-top:0">Funnel</h3>
    <div class="tools"><input class="in" v-model="funnel" style="min-width:360px" placeholder="event names in order, comma-separated" @keyup.enter="runFunnel" /><button class="btn" @click="runFunnel">Compute</button><span class="dim">users who did each step in order within {{ days }} days</span></div>
    <DataTable :cols="[{ key: 'step', mono: true }, { key: 'users' }, { key: 'conversion_from_previous', label: 'from previous', fmt: (r) => pct(r.conversion_from_previous) }, { key: 'conversion_from_start', label: 'from start', fmt: (r) => pct(r.conversion_from_start) }]" :rows="x.fun?.steps ?? []" empty="No steps" /></div>
  <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:12px">
    <div class="card"><h3 style="margin-top:0">Geography <small>({{ x.geo?.total ?? 0 }} accounts)</small></h3><DataTable :cols="[{ key: 'country', mono: true }, { key: 'accounts' }, { key: 'share', fmt: (r) => pct(r.accounts / Math.max(1, x.geo?.total ?? 1)) }]" :rows="(x.geo?.countries ?? []).slice(0, 15)" empty="No country data (GeoIP off or no sign-ins)" /></div>
    <div class="card"><h3 style="margin-top:0">Attribution <small>({{ x.attr?.touch }} touch by {{ x.attr?.group_by }})</small></h3><DataTable :cols="[{ key: 'group' }, { key: 'installs', fmt: (r) => r.installs ?? r.users ?? r.count }, { key: 'revenue' }]" :rows="x.attr?.groups ?? []" empty="No attribution data — clients send campaign / source on first open" /></div>
    <div class="card"><h3 style="margin-top:0">Sampling & reports</h3>
      <Kv k="Global sample"><span style="display:flex;gap:8px;align-items:center"><input class="in" type="number" min="1" max="100" v-model.number="samplePct" style="max-width:90px" /> % <button class="btn s" @click="saveSampling">Save</button></span></Kv>
      <Kv k="Always kept" :v="(x.samp?.always_enabled ?? []).join(', ') || 'none'" mono />
      <Kv k="Latest daily report" :v="x.rep?.for_date ? `${x.rep.for_date} · generated ${String(x.rep.generated_at).slice(0, 16).replace('T', ' ')}` : 'none yet'" />
      <router-link to="/analytics/reports" class="dim" style="font-size:12px">all reports →</router-link>
    </div>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
