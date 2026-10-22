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
const s = ref<any>(null); const err = ref(''); const tracks = ref<any[]>([]); const tables = ref<string[]>([]); const starts = ref(''); const ends = ref(''); const dlg = ref(false); const acc = ref('')
interface L { level: number; free: string; premium: string }
const levels = ref<L[]>([])
const toIso = (v: any) => v ? new Date(v).toISOString().slice(0, 16) : ''
async function load() {
  err.value = ''
  try {
    if (isNew.value) s.value = { id: '', name: '', track_id: '' }
    else { const r = await get('/v1/server/seasons'); s.value = (r.seasons ?? []).find((x: any) => x.id === id.value); if (!s.value) throw new Error('season not found') }
    levels.value = (s.value.levels ?? []).map((l: any) => ({ level: l.level, free: l.free ?? '', premium: l.premium ?? '' })); starts.value = toIso(s.value.starts_at); ends.value = toIso(s.value.ends_at)
  } catch (e: any) { err.value = e.message }
  try { const [t, r] = await Promise.all([get('/v1/server/progression/tracks'), get('/v1/server/rewards/tables')]); tracks.value = t.tracks ?? []; tables.value = (r.tables ?? []).map((x: any) => x.id) } catch {}
}
onMounted(load)
const track = computed(() => tracks.value.find((t) => t.id === s.value?.track_id))
function syncLevels() { const n = track.value?.levels?.length ?? 0; while (levels.value.length < n) levels.value.push({ level: levels.value.length + 1, free: '', premium: '' }) }
async function save() {
  try {
    const b = { ...s.value, id: s.value.id || undefined, starts_at: starts.value ? new Date(starts.value).toISOString() : null, ends_at: ends.value ? new Date(ends.value).toISOString() : null, levels: levels.value.map((l, i) => ({ level: i + 1, free: l.free || null, premium: l.premium || null })) }
    const r = await post('/v1/server/seasons', b); toast.show('season saved')
    if (isNew.value) router.replace(`/economy/seasons/${encodeURIComponent(r.id ?? r.season?.id ?? b.id)}`); else load()
  } catch (e: any) { toast.err(e) }
}
async function premium() { try { await post(`/v1/server/seasons/${encodeURIComponent(id.value)}/premium`, { account_id: acc.value }); toast.show('premium granted'); dlg.value = false } catch (e: any) { toast.err(e) } }
const state = computed(() => { if (!s.value?.starts_at) return 'draft'; const n = Date.now(); return n < +new Date(s.value.starts_at) ? 'upcoming' : s.value.ends_at && n > +new Date(s.value.ends_at) ? 'ended' : 'live' })
</script>
<template>
  <div class="crumb"><router-link to="/economy/seasons">Seasons</router-link> › <span>{{ isNew ? 'new season' : s?.name || id }}</span><span class="id" v-if="!isNew">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="!isNew" @click="dlg = true">Grant premium</button><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="s">
    <div class="grid">
      <div><Kv k="Id"><input class="in m" v-model="s.id" :disabled="!isNew" placeholder="auto" /></Kv><Kv k="Name"><input class="in" v-model="s.name" /></Kv><Kv k="State"><span :class="{ ok: state === 'live' }">{{ state }}</span></Kv></div>
      <div><Kv k="XP track (levels come from here)"><select class="sel" v-model="s.track_id" @change="syncLevels"><option value="">—</option><option v-for="t in tracks" :key="t.id" :value="t.id">{{ t.name }} · {{ (t.levels ?? []).length }} levels</option></select></Kv><Kv k="Track levels" :v="track ? (track.levels ?? []).map((l: any) => l.xp).join(' / ') + ' xp' : '—'" /></div>
      <div><Kv k="Starts"><input class="in" type="datetime-local" v-model="starts" /></Kv><Kv k="Ends"><input class="in" type="datetime-local" v-model="ends" /></Kv></div>
    </div>
    <h3>Level rewards <small>({{ levels.length }} — reward table per tier)</small> <button class="btn s p" style="margin-left:8px" @click="levels.push({ level: levels.length + 1, free: '', premium: '' })">Add level</button></h3>
    <table><thead><tr><th style="width:70px">Level</th><th style="width:120px">XP</th><th>Free track</th><th>Premium track</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(l, i) in levels" :key="i"><td class="dim">{{ i + 1 }}</td><td class="dim">{{ track?.levels?.[i]?.xp ?? '—' }}</td><td><input class="in m" style="min-width:0;width:100%" v-model="l.free" list="tables" placeholder="none" /></td><td><input class="in m" style="min-width:0;width:100%" v-model="l.premium" list="tables" placeholder="none" /></td><td><button class="btn s d" @click="levels.splice(i, 1)">×</button></td></tr>
      <tr v-if="!levels.length"><td colspan="5" class="empty">No levels — pick a track or add levels</td></tr></tbody></table>
    <datalist id="tables"><option v-for="t in tables" :key="t" :value="t" /></datalist>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="s" />
  </div>
  <Drawer v-if="dlg" title="Grant premium pass" @close="dlg = false"><div class="form"><div class="f"><label>Account id</label><input class="in m" v-model="acc" /></div><div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" :disabled="!acc" @click="premium">Grant</button></div></div></Drawer>
</template>
