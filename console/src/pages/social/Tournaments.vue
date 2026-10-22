<script setup lang="ts">
// Tournaments and their brackets: create, build a bracket (single/double elimination, swiss,
// round robin), advance rounds, report or forfeit matches, end.
import { ref, onMounted } from 'vue'
import { api, get, post, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref(''); const form = ref<any>({}); const cur = ref<any>(null)
async function load() { err.value = ''; try { const r = await get('/v1/server/tournaments' + qs({ limit: 100 })); x.value.t = Array.isArray(r) ? { tournaments: r } : r } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; await load(); if (cur.value) open(cur.value.id); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
const list = () => x.value.t?.tournaments ?? x.value.t?.items ?? []
async function open(id: string) { cur.value = list().find((t: any) => t.id === id) ?? { id }; x.value.b = await get(`/v1/server/tournaments/${id}/bracket`).catch((e) => (e.status === 404 ? null : { error: e.message })); x.value.rec = await get(`/v1/server/tournaments/${id}/records?limit=50`).catch(() => null) }
const matches = () => x.value.b?.matches ?? x.value.b?.rounds?.flatMap((r: any) => r.matches ?? []) ?? []
function newT() { dlg.value = 'new'; form.value = { text: JSON.stringify({ title: 'Weekend Cup', description: '', category: 1, sort_order: 'desc', operator: 'best', max_size: 64, max_num_score: 1, start_time: Math.floor(Date.now() / 1000), end_time: Math.floor(Date.now() / 1000) + 86400 * 2, duration: 86400 * 2, join_required: true, metadata: {} }, null, 2) } }
function newBracket() { dlg.value = 'bracket'; form.value = { format: 'single_elimination', seeding: 'ranked', participants: '', size: 8, swiss_rounds: 3, deadline_secs: 3600 } }
async function createBracket() { const f = form.value; await act(() => post(`/v1/server/tournaments/${cur.value.id}/bracket`, { format: f.format, seeding: f.seeding, participants: f.participants ? f.participants.split(/[\s,]+/).filter(Boolean) : undefined, size: Number(f.size) || undefined, swiss_rounds: f.format === 'swiss' ? Number(f.swiss_rounds) : undefined, deadline_secs: Number(f.deadline_secs) || undefined }), 'bracket created') }
function report(m: any) { dlg.value = 'report'; form.value = { mid: m.id ?? m.match_id, winner: m.a ?? m.participants?.[0] ?? '', score: '', a: m.a ?? m.participants?.[0], b: m.b ?? m.participants?.[1] } }
</script>
<template>
  <div class="crumb"><span>Tournaments</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="newT">New tournament</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <DataTable :cols="[{ key: 'id', mono: true }, { key: 'title' }, { key: 'category' }, { key: 'size', fmt: (r) => `${r.size ?? r.participants ?? 0} / ${r.max_size ?? '∞'}` }, { key: 'start_time', fmt: (r) => ts(r.start_time) }, { key: 'end_time', fmt: (r) => ts(r.end_time) }, { key: 'state', fmt: (r) => r.ended ? 'ended' : r.end_time && r.end_time * 1000 < Date.now() ? 'over' : r.start_time && r.start_time * 1000 > Date.now() ? 'upcoming' : 'live' }]" :rows="list()" pick @pick="(r) => open(r.id)" empty="No tournaments — create one; players join and write scores through the client API" />
  </div>
  <Drawer v-if="cur" :title="cur.title ?? cur.id" @close="cur = null">
    <div class="tools"><button class="btn s p" v-if="!x.b" @click="newBracket">Create bracket</button><button class="btn s" v-if="x.b && !x.b.error" @click="act(() => post(`/v1/server/tournaments/${cur.id}/bracket/rounds`), 'next round generated')">Next round</button><button class="btn s d" v-if="x.b && !x.b.error" @click="ask('Delete the bracket? Reported results are lost.') && act(() => api('DELETE', `/v1/server/tournaments/${cur.id}/bracket`), 'bracket deleted')">Delete bracket</button><button class="btn s d" @click="ask('End the tournament now and pay out?') && act(() => post(`/v1/server/tournaments/${cur.id}/end`), 'ended')">End</button><button class="btn s d" @click="ask(`Delete tournament ${cur.id}?`) && act(async () => { await api('DELETE', `/v1/server/tournaments/${cur.id}`); cur = null }, 'deleted')">Delete</button></div>
    <Kv k="Id" :v="cur.id" mono copy /><Kv k="Window" :v="`${ts(cur.start_time)} → ${ts(cur.end_time)}`" /><Kv k="Scoring" :v="`${cur.operator ?? '—'} · ${cur.sort_order ?? '—'} · ${cur.max_num_score ?? '—'} scores`" />
    <template v-if="x.b && !x.b.error">
      <h3>Bracket <small>{{ x.b.format }} · round {{ x.b.round ?? x.b.current_round ?? '?' }}{{ x.b.rounds_total ? ' of ' + x.b.rounds_total : '' }}</small></h3>
      <DataTable :cols="[{ key: 'round' }, { key: 'a', fmt: (r) => r.a ?? r.participants?.[0] ?? 'bye', mono: true }, { key: 'b', fmt: (r) => r.b ?? r.participants?.[1] ?? 'bye', mono: true }, { key: 'winner', mono: true, fmt: (r) => r.winner ?? (r.forfeit ? 'forfeit' : '') }, { key: 'score', fmt: (r) => r.score ?? '' }, { key: 'deadline', fmt: (r) => ts(r.deadline), dim: true }]" :rows="matches()" empty="No matches in this bracket">
        <template #actions="{ row }"><template v-if="!row.winner"><button class="btn s" @click="report(row)">Report</button><button class="btn s d" @click="form = { mid: row.id ?? row.match_id, participant: row.a ?? row.participants?.[0] }; dlg = 'forfeit'">Forfeit</button></template></template>
      </DataTable>
      <h3>Standings / raw</h3><JsonView :value="x.b.standings ?? x.b" />
    </template>
    <p class="dim" v-else-if="x.b?.error">{{ x.b.error }}</p>
    <p class="dim" v-else>No bracket — the tournament is a leaderboard only until you create one.</p>
    <template v-if="x.rec"><h3>Records</h3><JsonView :value="x.rec" /></template>
  </Drawer>
  <Drawer v-if="dlg === 'new'" title="New tournament" @close="dlg = ''"><div class="form">
    <div class="f"><label>Definition (JSON)</label><textarea class="in" v-model="form.text" style="min-height:260px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post('/v1/server/tournaments', JSON.parse(form.text)), 'created')">Create</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'bracket'" title="Create bracket" @close="dlg = ''"><div class="form">
    <div class="f"><label>Format</label><select class="in" v-model="form.format"><option value="single_elimination">single elimination</option><option value="double_elimination">double elimination</option><option value="swiss">swiss</option><option value="round_robin">round robin</option></select></div>
    <div class="f"><label>Seeding</label><select class="in" v-model="form.seeding"><option value="ranked">by leaderboard rank</option><option value="random">random</option><option value="manual">manual (order below)</option></select></div>
    <div class="f"><label>Participants (ids, optional — default: everyone who joined)</label><textarea class="in" v-model="form.participants" style="min-height:60px"></textarea></div>
    <div class="f"><label>Size / swiss rounds / match deadline (s)</label><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="form.size" /><input class="in" type="number" v-model.number="form.swiss_rounds" /><input class="in" type="number" v-model.number="form.deadline_secs" /></div></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="createBracket">Create</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'report'" title="Report result" @close="dlg = ''"><div class="form">
    <div class="f"><label>Winner</label><select class="in" v-model="form.winner"><option :value="form.a">{{ form.a }}</option><option :value="form.b">{{ form.b }}</option></select></div>
    <div class="f"><label>Score (free text, e.g. 2-1)</label><input class="in" v-model="form.score" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`/v1/server/tournaments/${cur.id}/bracket/matches/${form.mid}`, { winner: form.winner, score: form.score || undefined }), 'reported')">Report</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'forfeit'" title="Forfeit" @close="dlg = ''"><div class="form">
    <div class="f"><label>Participant who forfeits</label><input class="in" v-model="form.participant" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn d" @click="act(() => post(`/v1/server/tournaments/${cur.id}/bracket/matches/${form.mid}/forfeit`, { participant: form.participant }), 'forfeited')">Forfeit</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
