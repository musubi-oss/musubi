<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new')
const t = ref<any>(null); const err = ref(''); const tables = ref<string[]>([]); const dlg = ref(false); const form = ref({ account_id: '', xp: 100, reason: 'console' })
interface L { xp: number; rewards: string }
const levels = ref<L[]>([])
async function load() {
  err.value = ''
  try {
    if (isNew.value) t.value = { id: '', name: '' }
    else { const r = await get('/v1/server/progression/tracks'); t.value = (r.tracks ?? []).find((x: any) => x.id === id.value); if (!t.value) throw new Error('track not found') }
    levels.value = (t.value.levels ?? []).map((l: any) => ({ xp: l.xp, rewards: l.rewards ? (typeof l.rewards === 'string' ? l.rewards : JSON.stringify(l.rewards)) : '' }))
  } catch (e: any) { err.value = e.message }
  try { tables.value = ((await get('/v1/server/rewards/tables')).tables ?? []).map((x: any) => x.id) } catch {}
}
onMounted(load)
const cum = computed(() => { let s = 0; return levels.value.map((l) => (s += Number(l.xp || 0))) })
function parseRewards(x: string) { if (!x.trim()) return null; try { return JSON.parse(x) } catch { return x } }
async function save() {
  try {
    const b = { ...t.value, id: t.value.id || undefined, levels: levels.value.map((l) => ({ xp: Number(l.xp), rewards: parseRewards(l.rewards) })) }
    const r = await post('/v1/server/progression/tracks', b); toast.show('track saved')
    if (isNew.value) router.replace(`/economy/progression/${encodeURIComponent(r.id ?? r.track?.id ?? b.id)}`); else load()
  } catch (e: any) { toast.err(e) }
}
async function grant() { try { await post('/v1/server/progression/xp', { ...form.value, track_id: id.value, xp: Number(form.value.xp) }); toast.show('xp granted'); dlg.value = false } catch (e: any) { toast.err(e) } }
</script>
<template>
  <div class="crumb"><router-link to="/economy/progression">Progression</router-link> › <span>{{ isNew ? 'new track' : t?.name || id }}</span><span class="id" v-if="!isNew">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="!isNew" @click="dlg = true">Grant XP</button><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="t">
    <div class="grid"><div><Kv k="Id"><input class="in m" v-model="t.id" :disabled="!isNew" placeholder="auto" /></Kv></div><div><Kv k="Name"><input class="in" v-model="t.name" placeholder="Account" /></Kv></div><div><Kv k="Levels / total xp" :v="`${levels.length} / ${cum[cum.length - 1] ?? 0}`" /></div></div>
    <h3>Levels <small>(xp needed to reach each level)</small> <button class="btn s p" style="margin-left:8px" @click="levels.push({ xp: levels.length ? levels[levels.length - 1].xp * 2 : 100, rewards: '' })">Add level</button></h3>
    <table><thead><tr><th style="width:70px">Level</th><th style="width:140px">XP to reach</th><th style="width:140px">Cumulative</th><th>Rewards (reward table id or JSON)</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(l, i) in levels" :key="i"><td class="dim">{{ i + 1 }}</td><td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="l.xp" /></td><td class="dim">{{ cum[i] }}</td><td><input class="in m" style="min-width:0;width:100%" v-model="l.rewards" list="tables" placeholder="none" /></td><td><button class="btn s d" @click="levels.splice(i, 1)">×</button></td></tr>
      <tr v-if="!levels.length"><td colspan="5" class="empty">No levels</td></tr></tbody></table>
    <datalist id="tables"><option v-for="x in tables" :key="x" :value="x" /></datalist>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="t" />
  </div>
  <Drawer v-if="dlg" title="Grant XP" @close="dlg = false"><div class="form"><div class="f"><label>Account id</label><input class="in m" v-model="form.account_id" /></div><div class="f"><label>XP</label><input class="in" type="number" v-model.number="form.xp" /></div><div class="f"><label>Reason</label><input class="in" v-model="form.reason" /></div><div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" :disabled="!form.account_id" @click="grant">Grant</button></div></div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
