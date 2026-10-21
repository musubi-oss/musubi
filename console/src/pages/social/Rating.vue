<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const queues = ref<any[]>([]); const err = ref(""); const dlg = ref(""); const q = ref<any>(null); const sel = ref<any>(null); const isNew = ref(false); const soft = ref(true)
async function load() { err.value = ''; try { queues.value = (await get('/v1/server/rating/queues')).queues ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
function edit(x: any) { isNew.value = !x; q.value = JSON.parse(JSON.stringify(x ?? { id: '', system: 'glicko2', params: { elo_k: 32, glicko_tau: 0.5, openskill_beta: null, placement_games: 10, placement_k_mult: 2, decay_after_secs: 1209600, decay_every_secs: 604800, decay_points: 25, decay_floor: null, season_carry: 0.5 } })); q.value.params ||= {}; dlg.value = 'edit' }
async function save() { try { const { id, created_at: _c, updated_at: _u, season: _s, ...rest } = q.value; await put(`/v1/server/rating/queues/${encodeURIComponent(id)}`, rest); toast.show('rating queue saved'); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
const days = (s: any) => s ? `${Math.round(s / 86400)} d` : 'off'
const ts = (v: any) => v ? String(v).replace('T', ' ').slice(0, 16) : '—'
</script>
<template>
  <div class="crumb"><span>Rating queues</span><span class="id">{{ queues.length }} · skill ratings per matchmaking queue</span><span style="margin-left:auto"><button class="btn s p" @click="edit(null)">New rating queue</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Match results reported for a queue update each player's rating with the chosen system; placement games use a larger K, inactive players decay, seasons soft-reset towards the initial rating.</p>
    <table><thead><tr><th>Queue</th><th style="width:100px">System</th><th style="width:70px">Season</th><th>Parameters</th><th style="width:170px">Decay</th><th style="width:140px">Updated</th><th style="width:230px"></th></tr></thead><tbody>
      <tr v-for="x in queues" :key="x.id"><td class="m row" @click="edit(x)">{{ x.id }}</td><td>{{ x.system }}</td><td>{{ x.season }}</td><td class="dim">{{ x.system === 'elo' ? `K ${x.params?.elo_k}` : x.system === 'glicko2' ? `τ ${x.params?.glicko_tau}` : `β ${x.params?.openskill_beta ?? '—'}` }} · placement {{ x.params?.placement_games ?? 0 }} games ×{{ x.params?.placement_k_mult ?? 1 }} · carry {{ x.params?.season_carry ?? 0 }}</td><td class="dim">{{ x.params?.decay_after_secs ? `after ${days(x.params.decay_after_secs)}, −${x.params.decay_points} every ${days(x.params.decay_every_secs)}${x.params.decay_floor != null ? ` to ${x.params.decay_floor}` : ''}` : 'off' }}</td><td class="dim">{{ ts(x.updated_at) }}</td>
        <td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" @click="sel = x; dlg = 'season'; soft = true">New season</button> <button class="btn s" @click="act(() => post(`/v1/server/rating/queues/${encodeURIComponent(x.id)}/decay`), 'decay applied')">Decay now</button> <button class="btn s" @click="ask(`Delete rating queue ${x.id} and every rating in it?`) && act(() => api('DELETE', `/v1/server/rating/queues/${encodeURIComponent(x.id)}`), 'deleted')">×</button></td></tr>
      <tr v-if="!queues.length"><td colspan="7" class="empty">No rating queues</td></tr></tbody></table>
  </div>
  <Drawer v-if="dlg === 'edit'" :title="isNew ? 'New rating queue' : `Rating ${q.id}`" @close="dlg = ''"><div class="form">
    <div class="f"><label>Queue id (matches the matchmaking queue name)</label><input class="in m" v-model="q.id" :disabled="!isNew" /></div>
    <div class="f"><label>System</label><select class="sel" v-model="q.system"><option value="glicko2">Glicko-2 — rating + deviation + volatility</option><option value="elo">Elo — simple K-factor</option><option value="openskill">OpenSkill — team-aware (TrueSkill-like)</option></select></div>
    <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div v-if="q.system === 'elo'"><label>K</label><input class="in" type="number" v-model.number="q.params.elo_k" style="width:100px" /></div><div v-if="q.system === 'glicko2'"><label>τ (volatility)</label><input class="in" type="number" step="0.1" v-model.number="q.params.glicko_tau" style="width:100px" /></div><div v-if="q.system === 'openskill'"><label>β</label><input class="in" type="number" step="0.1" v-model.number="q.params.openskill_beta" style="width:100px" /></div><div><label>Placement games</label><input class="in" type="number" min="0" v-model.number="q.params.placement_games" style="width:120px" /></div><div><label>Placement K ×</label><input class="in" type="number" step="0.1" v-model.number="q.params.placement_k_mult" style="width:110px" /></div></div>
    <h3>Inactivity decay</h3>
    <div class="f" style="display:flex;gap:8px;flex-wrap:wrap"><div><label>After (s, 0 = off)</label><input class="in" type="number" min="0" v-model.number="q.params.decay_after_secs" style="width:130px" /></div><div><label>Every (s)</label><input class="in" type="number" min="0" v-model.number="q.params.decay_every_secs" style="width:120px" /></div><div><label>Points</label><input class="in" type="number" v-model.number="q.params.decay_points" style="width:100px" /></div><div><label>Floor</label><input class="in" type="number" v-model.number="q.params.decay_floor" placeholder="—" style="width:100px" /></div></div>
    <div class="f"><label>Season carry-over (0 = full reset, 1 = keep rating)</label><input class="in" type="number" min="0" max="1" step="0.05" v-model.number="q.params.season_carry" style="width:120px" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!q.id" @click="save">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'season'" title="Start a new rating season" @close="dlg = ''"><div class="form"><p class="note">Archives current ratings as the finished season. Soft reset pulls every rating towards the initial value by the carry-over factor; hard reset starts everyone over.</p><div class="f"><label><input type="checkbox" v-model="soft" /> soft reset</label></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`/v1/server/rating/queues/${encodeURIComponent(sel.id)}/season`, { soft_reset: soft }), 'new season started')">Start season</button></div></div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
