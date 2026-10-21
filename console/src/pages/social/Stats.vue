<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const defs = ref<any[]>([]); const err = ref(''); const q = ref(''); const dlg = ref(''); const d = ref<any>(null); const isNew = ref(false); const cycles = ref<{ name: string; period: string }[]>([]); const meta = ref('{}'); const upd = ref({ account_id: '', stat: '', value: 0, op: 'set' }); const board = ref<any>(null)
async function load() { err.value = ''; try { defs.value = (await get('/v1/server/stats/defs')).defs ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
const view = computed(() => defs.value.filter((x) => !q.value || JSON.stringify(x).toLowerCase().includes(q.value.toLowerCase())))
function edit(x: any) { isNew.value = !x; d.value = JSON.parse(JSON.stringify(x ?? { id: '', name: '', kind: 'int', global: false, visibility: 'public', default_value: 0, increment_only: false, client_writable: false, min_value: null, max_value: null, max_change: null, window_secs: null, cycles: [], metadata: {} })); cycles.value = (d.value.cycles ?? []).map((c: any) => typeof c === 'string' ? { name: c, period: c } : { name: c.name ?? '', period: c.period ?? c.kind ?? 'weekly' }); meta.value = JSON.stringify(d.value.metadata ?? {}, null, 2); dlg.value = 'edit' }
async function save() { try { await put('/v1/server/stats/defs', { ...d.value, cycles: cycles.value.filter((c) => c.name).map((c) => ({ name: c.name, period: c.period })), metadata: JSON.parse(meta.value || '{}'), default_value: Number(d.value.default_value || 0), min_value: d.value.min_value ?? null, max_value: d.value.max_value ?? null, max_change: d.value.max_change ?? null, window_secs: d.value.window_secs || null }); toast.show('stat saved'); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
async function update() { try { await post('/v1/server/stats/update', { ...upd.value, value: Number(upd.value.value) }); toast.show('stat updated'); dlg.value = '' } catch (x: any) { toast.err(x) } }
async function del(x: any) { try { await api('DELETE', `/v1/server/stats/defs/${encodeURIComponent(x.id)}`); toast.show('deleted'); load() } catch (e: any) { toast.err(e) } }
async function top(x: any) { board.value = null; try { board.value = await get(`/v1/server/leaderboards/${encodeURIComponent(x.id)}?limit=20`) } catch { board.value = await get(`/v1/server/stats/${encodeURIComponent(x.id)}/top?limit=20`).catch((e) => ({ error: e.message })) } }
const kinds = [['int', 'int — counter / score'], ['float', 'float'], ['max', 'max — keeps the highest value'], ['min', 'min — keeps the lowest'], ['avgrate', 'avgrate — rolling average per window'], ['set', 'set — last written value']]
</script>
<template>
  <div class="crumb"><span>Stats &amp; leaderboards</span><span class="id">{{ defs.length }} definitions</span>
    <span style="margin-left:auto;display:flex;gap:8px"><input class="in" v-model="q" placeholder="filter" style="width:200px" /><button class="btn s" @click="upd = { account_id: '', stat: defs[0]?.id ?? '', value: 0, op: 'set' }; dlg = 'update'">Update a stat</button><button class="btn s p" @click="edit(null)">New stat</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">A stat definition is also a leaderboard: every stat ranks players unless private. Cycles create periodic boards (daily / weekly / seasonal) that reset and archive.</p>
    <table><thead><tr><th>Id / name</th><th style="width:90px">Kind</th><th style="width:90px">Scope</th><th style="width:90px">Visibility</th><th style="width:150px">Rules</th><th style="width:140px">Cycles</th><th style="width:170px"></th></tr></thead><tbody>
      <tr v-for="x in view" :key="x.id"><td class="row" @click="edit(x)"><span class="m">{{ x.id }}</span><span class="dim" v-if="x.name"> — {{ x.name }}</span></td><td>{{ x.kind }}{{ x.kind === 'avgrate' && x.window_secs ? ` / ${x.window_secs}s` : '' }}</td><td class="dim">{{ x.global ? 'global' : 'per player' }}</td><td :class="{ dim: x.visibility !== 'public' }">{{ x.visibility }}</td><td class="dim">{{ [x.increment_only ? 'increment only' : null, x.client_writable ? 'client writes' : null, x.min_value != null || x.max_value != null ? `${x.min_value ?? '−∞'}…${x.max_value ?? '∞'}` : null, x.max_change != null ? `Δ≤${x.max_change}` : null].filter(Boolean).join(' · ') || '—' }}</td><td class="dim">{{ (x.cycles ?? []).map((c: any) => typeof c === 'string' ? c : c.name ?? c.period).join(', ') || '—' }}</td>
        <td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" v-if="x.visibility !== 'private'" @click="top(x)">Top 20</button> <button class="btn s" @click="ask(`Delete stat ${x.id} and its leaderboard?`) && del(x)">×</button></td></tr>
      <tr v-if="!view.length"><td colspan="7" class="empty">No stat definitions</td></tr></tbody></table>
    <template v-if="board"><h3>Leaderboard</h3><JsonView :value="board" /></template>
  </div>
  <Drawer v-if="dlg === 'edit'" :title="isNew ? 'New stat' : `Stat ${d.id}`" @close="dlg = ''"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Id</label><input class="in m" v-model="d.id" :disabled="!isNew" placeholder="kills, best_lap_ms" /></div><div style="flex:1"><label>Display name</label><input class="in" v-model="d.name" /></div></div>
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Kind</label><select class="sel" v-model="d.kind"><option v-for="[k, l] in kinds" :key="k" :value="k">{{ l }}</option></select></div><div v-if="d.kind === 'avgrate'"><label>Window (s)</label><input class="in" type="number" min="1" v-model.number="d.window_secs" style="width:120px" /></div></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Visibility</label><select class="sel" v-model="d.visibility"><option value="public">public — ranked, visible to all</option><option value="friends">friends — ranked among friends</option><option value="private">private — owner only, no board</option></select></div><div><label>Default</label><input class="in" type="number" v-model.number="d.default_value" style="width:110px" /></div></div>
    <div class="f"><label><input type="checkbox" v-model="d.global" /> global (one value for the whole game, not per player)</label><br /><label><input type="checkbox" v-model="d.increment_only" /> increment only (clients cannot lower it)</label><br /><label><input type="checkbox" v-model="d.client_writable" /> clients may write directly (else server / hooks only)</label></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Min</label><input class="in" type="number" v-model.number="d.min_value" placeholder="—" style="width:110px" /></div><div><label>Max</label><input class="in" type="number" v-model.number="d.max_value" placeholder="—" style="width:110px" /></div><div><label>Max change per write (anti-cheat)</label><input class="in" type="number" v-model.number="d.max_change" placeholder="—" style="width:160px" /></div></div>
    <h3>Cycles <small>(periodic boards)</small> <button class="btn s p" style="margin-left:8px" @click="cycles.push({ name: 'weekly', period: 'weekly' })">Add</button></h3>
    <div class="f" v-for="(c, i) in cycles" :key="i" style="display:flex;gap:8px"><input class="in m" v-model="c.name" placeholder="name" style="width:140px" /><select class="sel" v-model="c.period"><option>daily</option><option>weekly</option><option>monthly</option><option>season</option></select><button class="btn s" @click="cycles.splice(i, 1)">×</button></div>
    <div class="f"><label>Metadata (JSON)</label><textarea class="in m" v-model="meta" style="min-height:60px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!d.id" @click="save">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'update'" title="Update a player's stat" @close="dlg = ''"><div class="form">
    <div class="f"><label>Account id</label><input class="in m" v-model="upd.account_id" /></div><div class="f"><label>Stat</label><select class="sel" v-model="upd.stat"><option v-for="x in defs" :key="x.id" :value="x.id">{{ x.id }}</option></select></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Operation</label><select class="sel" v-model="upd.op"><option value="set">set</option><option value="incr">increment</option><option value="max">max</option><option value="min">min</option></select></div><div><label>Value</label><input class="in" type="number" v-model.number="upd.value" style="width:140px" /></div></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!upd.account_id || !upd.stat" @click="update">Apply</button></div></div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
