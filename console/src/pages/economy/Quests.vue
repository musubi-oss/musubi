<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { useRoute } from 'vue-router'
import { ref, computed, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const tab = ref<'quests' | 'pools'>((['quests', 'pools'] as const).find((k) => k === useRoute().query.tab) ?? 'quests'); const quests = ref<any[]>([]); const pools = ref<any[]>([]); const tables = ref<any[]>([]); const err = ref(''); const q = ref(''); const dlg = ref(''); const x = ref<any>(null); const isNew = ref(false); const ev = ref({ account_id: '', event: '', amount: 1 }); const preview = ref<any>(null); const acc = ref('')
async function load() { err.value = ''; const r = await Promise.allSettled([get('/v1/server/quests'), get('/v1/server/quests/pools'), get('/v1/server/rewards/tables')]); quests.value = r[0].status === 'fulfilled' ? r[0].value.quests ?? [] : (err.value = (r[0] as any).reason.message, []); pools.value = r[1].status === 'fulfilled' ? r[1].value.pools ?? [] : []; tables.value = r[2].status === 'fulfilled' ? r[2].value.tables ?? r[2].value.reward_tables ?? [] : [] }
onMounted(load)
const vq = computed(() => quests.value.filter((a) => !q.value || JSON.stringify(a).toLowerCase().includes(q.value.toLowerCase())))
const vp = computed(() => pools.value.filter((a) => !q.value || JSON.stringify(a).toLowerCase().includes(q.value.toLowerCase())))
function editQuest(a: any) { isNew.value = !a; x.value = JSON.parse(JSON.stringify(a ?? { id: '', name: '', kind: 'daily', description: '', objectives: [{ event: '', target: 1 }], rewards: '', enabled: true, rerollable: false, rerolls_per_period: 1 })); x.value.objectives ||= []; dlg.value = 'quest' }
function editPool(a: any) { isNew.value = !a; x.value = JSON.parse(JSON.stringify(a ?? { id: '', name: '', description: '', enabled: true, ends_at: null, size: 3, rotation: 'daily', goals: [{ id: 'g1', name: '', description: '', objectives: [{ event: '', target: 1 }], rewards: '', weight: 1 }] })); x.value.goals ||= []; dlg.value = 'pool' }
const clean = (o: any) => ({ ...o, objectives: (o.objectives ?? []).filter((j: any) => j.event).map((j: any) => ({ event: j.event, target: Number(j.target) || 1 })) })
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
const saveQuest = () => act(() => isNew.value ? post('/v1/server/quests', clean(x.value)) : put(`/v1/server/quests/${encodeURIComponent(x.value.id)}`, clean(x.value)), 'quest saved')
const savePool = () => act(() => { const b = { ...x.value, goals: x.value.goals.map((g: any) => ({ ...clean(g), weight: Number(g.weight) || 1 })) }; return isNew.value ? post('/v1/server/quests/pools', b) : put(`/v1/server/quests/pools/${encodeURIComponent(b.id)}`, b) }, 'pool saved')
async function runPreview(p: any) { try { preview.value = await get(`/v1/server/quests/pools/${encodeURIComponent(p.id)}/preview/${encodeURIComponent(acc.value)}`) } catch (e: any) { toast.err(e) } }
const objs = (a: any) => (a.objectives ?? []).map((o: any) => `${o.event} ×${o.target ?? o.count}`).join(', ')
const ts = (v: any) => v ? new Date(typeof v === 'number' ? v * 1000 : v).toISOString().replace('T', ' ').slice(0, 16) : '—'
const pg2 = usePager(() => vq)
const pg1 = usePager(() => vp)
</script>
<template>
  <div class="crumb"><span>Quests</span><span class="id">{{ quests.length }} quests · {{ pools.length }} pools</span>
    <span style="margin-left:auto;display:flex;gap:8px"><input class="in" v-model="q" placeholder="filter" style="width:180px" /><button class="btn s" @click="ev = { account_id: '', event: '', amount: 1 }; dlg = 'event'">Emit event</button><button class="btn s p" @click="tab === 'quests' ? editQuest(null) : editPool(null)">New {{ tab === 'quests' ? 'quest' : 'pool' }}</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tabs"><a :class="{ on: tab === 'quests' }" @click="tab = 'quests'">Quests</a><a :class="{ on: tab === 'pools' }" @click="tab = 'pools'">Pools (rotating goals)</a></div>
    <template v-if="tab === 'quests'">
      <p class="note">A quest completes when every objective reaches its target from gameplay events (<code>POST /v1/quests/event</code> or server hooks); rewards are a reward table.</p>
      <table><thead><tr><th>Id / name</th><th style="width:80px">Kind</th><th>Objectives</th><th>Rewards</th><th style="width:110px">Reroll</th><th style="width:80px">Enabled</th><th style="width:100px"></th></tr></thead><tbody>
        <tr v-for="a in pg2.rows" :key="a.id"><td class="row" @click="editQuest(a)"><span class="m">{{ a.id }}</span><span class="dim" v-if="a.name"> — {{ a.name }}</span></td><td>{{ a.kind }}</td><td class="dim">{{ objs(a) }}</td><td class="m dim">{{ a.rewards }}</td><td class="dim">{{ a.rerollable ? `${a.rerolls_per_period ?? 1}/period` : 'no' }}</td><td :class="a.enabled ? 'ok' : 'dim'">{{ a.enabled ? 'yes' : 'no' }}</td><td><button class="btn s" @click="editQuest(a)">Edit</button> <button class="btn s" @click="ask(`Delete quest ${a.id}?`) && act(() => api('DELETE', `/v1/server/quests/${encodeURIComponent(a.id)}`), 'deleted')">×</button></td></tr>
        <tr v-if="!pg2.total"><td colspan="7" class="empty">No quests</td></tr></tbody></table><Pager :p="pg2" />
    </template>
    <template v-else>
      <p class="note">A pool hands each player <i>size</i> goals drawn by weight every rotation; accelerate skips the wait for one player, retire ends the pool.</p>
      <table><thead><tr><th>Id / name</th><th style="width:70px">Goals</th><th style="width:120px">Rotation</th><th style="width:60px">Gen</th><th style="width:140px">Ends</th><th style="width:80px">Enabled</th><th style="width:230px"></th></tr></thead><tbody>
        <tr v-for="p in pg1.rows" :key="p.id"><td class="row" @click="editPool(p)"><span class="m">{{ p.id }}</span><span class="dim" v-if="p.name"> — {{ p.name }}</span></td><td>{{ (p.goals ?? []).length }}</td><td class="dim">{{ p.size ?? '?' }} per {{ p.rotation ?? 'period' }}</td><td class="dim">{{ p.generation }}</td><td class="dim">{{ ts(p.ends_at) }}</td><td :class="p.enabled ? 'ok' : 'dim'">{{ p.enabled ? 'yes' : 'no' }}</td>
          <td><button class="btn s" @click="editPool(p)">Edit</button> <button class="btn s" @click="x = p; preview = null; dlg = 'preview'">Preview</button> <button class="btn s" @click="ask(`Retire pool ${p.id}?`) && act(() => post(`/v1/server/quests/pools/${encodeURIComponent(p.id)}/retire`), 'retired')">Retire</button> <button class="btn s" @click="ask('Delete pool?') && act(() => api('DELETE', `/v1/server/quests/pools/${encodeURIComponent(p.id)}`), 'deleted')">×</button></td></tr>
        <tr v-if="!pg1.total"><td colspan="7" class="empty">No pools</td></tr></tbody></table><Pager :p="pg1" />
    </template>
  </div>
  <Drawer v-if="dlg === 'quest'" :title="isNew ? 'New quest' : `Quest ${x.id}`" @close="dlg = ''"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Id</label><input class="in m" v-model="x.id" :disabled="!isNew" /></div><div style="flex:1"><label>Name</label><input class="in" v-model="x.name" /></div></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Kind</label><select class="sel" v-model="x.kind"><option>daily</option><option>weekly</option><option>story</option><option>event</option><option>achievement</option></select></div><div style="flex:1"><label>Rewards (reward table)</label><input class="in m" list="rtables" v-model="x.rewards" /></div></div>
    <div class="f"><label>Description</label><input class="in" v-model="x.description" /></div>
    <h3>Objectives <button class="btn s p" style="margin-left:8px" @click="x.objectives.push({ event: '', target: 1 })">Add</button></h3>
    <div class="f" v-for="(o, i) in x.objectives" :key="i" style="display:flex;gap:8px"><input class="in m" v-model="o.event" placeholder="event name (match_won, kills)" style="flex:1" /><input class="in" type="number" min="1" v-model.number="o.target" style="width:90px" /><button class="btn s" @click="x.objectives.splice(i, 1)">×</button></div>
    <div class="f"><label><input type="checkbox" v-model="x.enabled" /> enabled</label> &nbsp; <label><input type="checkbox" v-model="x.rerollable" /> rerollable</label> <input v-if="x.rerollable" class="in" type="number" min="1" v-model.number="x.rerolls_per_period" style="width:70px" /><span class="dim" v-if="x.rerollable"> per period</span></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!x.id" @click="saveQuest">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'pool'" :title="isNew ? 'New quest pool' : `Pool ${x.id}`" @close="dlg = ''"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Id</label><input class="in m" v-model="x.id" :disabled="!isNew" /></div><div style="flex:1"><label>Name</label><input class="in" v-model="x.name" /></div></div>
    <div class="f"><label>Description</label><input class="in" v-model="x.description" /></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Goals per player</label><input class="in" type="number" min="1" v-model.number="x.size" style="width:110px" /></div><div><label>Rotation</label><select class="sel" v-model="x.rotation"><option>daily</option><option>weekly</option><option>monthly</option></select></div><div><label>Ends at (unix s, empty = open)</label><input class="in" type="number" v-model.number="x.ends_at" style="width:160px" /></div><div><label>&nbsp;</label><label><input type="checkbox" v-model="x.enabled" /> enabled</label></div></div>
    <h3>Goals <small>({{ x.goals.length }})</small> <button class="btn s p" style="margin-left:8px" @click="x.goals.push({ id: `g${x.goals.length + 1}`, name: '', description: '', objectives: [{ event: '', target: 1 }], rewards: '', weight: 1 })">Add goal</button></h3>
    <div v-for="(g, gi) in x.goals" :key="gi" style="border:1px solid var(--line);border-radius:6px;padding:8px;margin-bottom:8px">
      <div class="f" style="display:flex;gap:8px"><input class="in m" v-model="g.id" placeholder="id" style="width:90px" /><input class="in" v-model="g.name" placeholder="name" style="flex:1" /><input class="in m" list="rtables" v-model="g.rewards" placeholder="reward table" style="flex:1" /><input class="in" type="number" min="1" v-model.number="g.weight" title="weight" style="width:70px" /><button class="btn s" @click="x.goals.splice(gi, 1)">×</button></div>
      <div class="f" v-for="(o, i) in g.objectives" :key="i" style="display:flex;gap:8px;margin:4px 0 0 16px"><input class="in m" v-model="o.event" placeholder="event" style="flex:1" /><input class="in" type="number" min="1" v-model.number="o.target" style="width:80px" /><button class="btn s" @click="g.objectives.splice(i, 1)">×</button></div>
      <button class="btn s" style="margin:4px 0 0 16px" @click="g.objectives.push({ event: '', target: 1 })">+ objective</button>
    </div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!x.id" @click="savePool">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'preview'" :title="`Pool ${x.id} — what a player gets`" @close="dlg = ''"><div class="form"><div class="f"><label>Account id</label><input class="in m" v-model="acc" /></div><div class="row"><button class="btn s" :disabled="!acc" @click="runPreview(x)">Preview</button><button class="btn s" :disabled="!acc" @click="act(() => post(`/v1/server/quests/pools/${encodeURIComponent(x.id)}/accelerate`, { account_id: acc }), 'rotation accelerated')">Accelerate rotation</button></div><JsonView v-if="preview" :value="preview" /></div></Drawer>
  <Drawer v-if="dlg === 'event'" title="Emit a quest event" @close="dlg = ''"><div class="form"><p class="note">Advances every active quest objective listening to this event for the player — handy to test a quest or compensate a player.</p><div class="f"><label>Account id</label><input class="in m" v-model="ev.account_id" /></div><div class="f" style="display:flex;gap:8px"><input class="in m" v-model="ev.event" placeholder="event" style="flex:1" /><input class="in" type="number" min="1" v-model.number="ev.amount" style="width:90px" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!ev.account_id || !ev.event" @click="act(() => post('/v1/server/quests/event', { ...ev, amount: Number(ev.amount) }), 'event emitted')">Emit</button></div></div></Drawer>
  <datalist id="rtables"><option v-for="t in tables" :key="t.id" :value="t.id" /></datalist>
</template>
