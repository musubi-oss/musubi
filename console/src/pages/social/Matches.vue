<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const router = useRouter(); const ask = (m: string) => window.confirm(m)
const tab = ref<'matches' | 'rejections' | 'stats' | 'turns'>('matches'); const matches = ref<any[]>([]); const rej = ref<any[]>([]); const stats = ref<any>(null); const turns = ref<any[]>([]); const err = ref(''); const sel = ref<any>(null); const trace = ref<any>(null); const tf = ref('active')
async function load() {
  err.value = ''
  const r = await Promise.allSettled([get('/v1/server/matchmaker/matches'), get('/v1/server/matchmaker/rejections'), get('/v1/server/matchmaker/stats'), get('/v1/server/turns')])
  const v = (i: number) => (r[i].status === 'fulfilled' ? (r[i] as any).value : (err.value += (r[i] as any).reason?.message + ' ', null))
  matches.value = v(0)?.matches ?? []; rej.value = v(1)?.rejections ?? []; stats.value = v(2); turns.value = v(3)?.matches ?? []
}
onMounted(load)
async function pickMatch(m: any) { sel.value = m; trace.value = null; const t = m.tickets?.[0]; if (t) trace.value = await get(`/v1/server/matchmaker/tickets/${t}/trace`).catch((e) => ({ error: e.message })) }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => !v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(v)).toISOString().replace('T', ' ').slice(0, 16)
const short = (s: any) => s ? String(s).slice(0, 8) + '…' : '—'
const avg = (a: number[]) => a?.length ? Math.round(a.reduce((x, y) => x + y, 0) / a.length) : 0
const turnState = (t: any) => t.finished_at ? 'finished' : t.current_seat == null ? 'waiting for players' : 'in progress'
</script>
<template>
  <div class="crumb"><span>Matches</span><span class="id" v-if="stats">{{ matches.length }} recent matches · {{ stats.open_matches ?? 0 }} open · {{ rej.length }} rejections · {{ turns.length }} turn-based</span><span style="margin-left:auto"><button class="btn s" @click="load">Refresh</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tabs"><a :class="{ on: tab === 'matches' }" @click="tab = 'matches'">Formed matches</a><a :class="{ on: tab === 'rejections' }" @click="tab = 'rejections'">Rejections</a><a :class="{ on: tab === 'stats' }" @click="tab = 'stats'">Queue stats</a><a :class="{ on: tab === 'turns' }" @click="tab = 'turns'">Turn-based</a></div>
    <template v-if="tab === 'matches'">
      <div style="display:grid;grid-template-columns:3fr 2fr;gap:12px">
        <table><thead><tr><th style="width:140px">Formed</th><th>Queue</th><th style="width:90px">Region</th><th style="width:80px">Players</th><th style="width:90px">Quality</th><th style="width:110px">MMR gap</th><th style="width:110px">Avg wait</th></tr></thead><tbody>
          <tr v-for="m in matches" :key="m.token" class="row" :class="{ on: sel?.token === m.token }" @click="pickMatch(m)"><td class="dim">{{ ts(m.created_at) }}</td><td class="m">{{ m.queue }}</td><td class="dim">{{ m.region }}</td><td>{{ (m.accounts ?? []).length }}</td><td><div class="bar" style="width:60px;display:inline-block;vertical-align:middle"><i :style="{ width: Math.round((m.quality ?? 0) * 100) + '%' }"></i></div> {{ ((m.quality ?? 0) * 100).toFixed(0) }}%</td><td class="dim">{{ m.team_mmr_gap ?? '—' }}</td><td class="dim">{{ avg(m.wait_ms) }} ms</td></tr>
          <tr v-if="!matches.length"><td colspan="7" class="empty">No matches formed recently</td></tr></tbody></table>
        <div v-if="sel"><h3 style="margin-top:0">Match {{ short(sel.token) }}</h3><Kv k="Players"><div v-for="a in sel.accounts" :key="a" class="m row" @click="router.push(`/players/accounts/${a}`)">{{ a }}</div></Kv><Kv k="Tickets" :v="(sel.tickets ?? []).map(short).join(', ')" mono /><Kv k="Wait per ticket" :v="(sel.wait_ms ?? []).map((w: number) => w + ' ms').join(' · ')" /><h3>Ticket trace</h3><JsonView :value="trace ?? {}" /></div>
        <div class="dim" v-else style="display:flex;align-items:center;justify-content:center">Select a match for players and the matcher's trace</div>
      </div>
    </template>
    <template v-else-if="tab === 'rejections'">
      <table><thead><tr><th style="width:140px">At</th><th>Ticket</th><th>Queue</th><th>Reason</th></tr></thead><tbody>
        <tr v-for="(r, i) in rej" :key="i"><td class="dim">{{ ts(r.at) }}</td><td class="m">{{ r.ticket_id }}</td><td class="m">{{ r.queue }}</td><td>{{ r.reason }}</td></tr>
        <tr v-if="!rej.length"><td colspan="4" class="empty">No rejected tickets — every ticket satisfied its queue rules</td></tr></tbody></table>
    </template>
    <template v-else-if="tab === 'stats'">
      <div class="grid" v-if="stats"><div><Kv k="Open matches" :v="stats.open_matches ?? 0" /><Kv k="Paused queues" :v="(stats.paused ?? []).join(', ') || 'none'" /></div><div><Kv k="Draining queues" :v="(stats.draining ?? []).join(', ') || 'none'" /><Kv k="Waiting tickets" :v="Object.values(stats.waiting ?? {}).reduce((a: number, b: any) => a + Number(b), 0)" /></div><div><Kv k="Per queue" :v="Object.entries(stats.waiting ?? {}).map(([q, n]) => `${q}: ${n}`).join(' · ') || '—'" /></div></div>
      <JsonView :value="stats ?? {}" />
    </template>
    <template v-else>
      <div style="display:flex;gap:8px;margin-bottom:10px"><select class="sel" v-model="tf"><option value="active">active</option><option value="finished">finished</option><option value="all">all</option></select><button class="btn s" @click="act(() => post('/v1/server/turns/sweep'), 'deadlines swept')">Sweep deadlines</button></div>
      <table><thead><tr><th>Game</th><th>Match</th><th style="width:90px">Players</th><th style="width:140px">State</th><th style="width:70px">Turn</th><th style="width:140px">Deadline</th><th style="width:140px">Created</th><th style="width:130px"></th></tr></thead><tbody>
        <tr v-for="t in turns.filter((t) => tf === 'all' || (tf === 'finished') === !!t.finished_at)" :key="t.id"><td class="m">{{ t.game }}</td><td class="m dim">{{ short(t.id) }}</td><td>{{ (t.players ?? []).length }} / {{ t.max_players }}</td><td :class="{ dim: !!t.finished_at }">{{ turnState(t) }}{{ t.result ? ` · ${typeof t.result === 'string' ? t.result : JSON.stringify(t.result)}` : '' }}</td><td>{{ (t.moves ?? []).length }}</td><td class="dim">{{ ts(t.deadline_at) }}</td><td class="dim">{{ ts(t.created_at) }}</td>
          <td><button class="btn s" v-if="!t.finished_at" @click="ask('Finish this match without a winner?') && act(() => post(`/v1/server/turns/${t.id}/finish`, { winner_seat: null }), 'finished')">Finish</button> <button class="btn s" @click="ask('Delete match?') && act(() => api('DELETE', `/v1/server/turns/${t.id}`), 'deleted')">×</button></td></tr>
        <tr v-if="!turns.length"><td colspan="8" class="empty">No turn-based matches</td></tr></tbody></table>
    </template>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
