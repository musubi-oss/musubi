<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new'); const E = computed(() => `/v1/server/live_events/${encodeURIComponent(id.value)}`)
const e = ref<any>(null); const err = ref(''); const audiences = ref<string[]>([]); const participants = ref<any[]>([]); const tab = ref<'phases' | 'participants'>('phases')
interface P { name: string; offset_secs: number; duration_secs: number; metadata: string }
const phases = ref<P[]>([]); const labels = ref(''); const starts = ref(''); const ends = ref(''); const meta = ref('{}'); const recur = ref<'none' | 'hourly' | 'daily' | 'weekly' | 'custom'>('none'); const recurSecs = ref(0)
const toIso = (v: any) => v ? new Date(v * 1000).toISOString().slice(0, 16) : ''
const fromIso = (x: string) => x ? Math.floor(new Date(x).getTime() / 1000) : null
async function load() {
  err.value = ''
  try {
    if (isNew.value) e.value = { name: '', description: '', enabled: true, participation: 'implicit', audience: null, local_time: false, max_participants: null, participation_secs: null, phases: [], metadata: {} }
    else { const r = await get(E.value); e.value = r.live_event ?? r.event ?? r }
    phases.value = (e.value.phases ?? []).map((p: any) => ({ name: p.name, offset_secs: p.offset_secs ?? 0, duration_secs: p.duration_secs ?? 0, metadata: p.metadata ? JSON.stringify(p.metadata) : '' }))
    labels.value = (e.value.labels ?? []).join(', '); starts.value = toIso(e.value.starts_at); ends.value = toIso(e.value.ends_at); meta.value = JSON.stringify(e.value.metadata ?? {}, null, 2)
    const rs = e.value.recurrence_secs; recur.value = !rs ? 'none' : rs === 3600 ? 'hourly' : rs === 86400 ? 'daily' : rs === 604800 ? 'weekly' : 'custom'; recurSecs.value = rs ?? 0
    if (!isNew.value) participants.value = ((await get(`${E.value}/participants`).catch(() => ({}))).participants ?? [])
  } catch (x: any) { err.value = x.message }
  try { audiences.value = ((await get('/v1/server/audiences')).audiences ?? []).map((a: any) => a.name) } catch {}
}
onMounted(load)
const total = computed(() => phases.value.reduce((m, p) => Math.max(m, Number(p.offset_secs) + Number(p.duration_secs)), 0))
const fmt = (s: number) => s >= 86400 ? `${(s / 86400).toFixed(s % 86400 ? 1 : 0)}d` : s >= 3600 ? `${(s / 3600).toFixed(s % 3600 ? 1 : 0)}h` : s >= 60 ? `${Math.round(s / 60)}m` : `${s}s`
const state = computed(() => { if (!e.value?.starts_at) return 'draft'; const n = Date.now() / 1000; if (!e.value.enabled) return 'disabled'; if (n < e.value.starts_at) return 'upcoming'; if (e.value.ends_at && n > e.value.ends_at) return 'ended'; return 'live' })
function body() {
  const rs = recur.value === 'none' ? null : recur.value === 'hourly' ? 3600 : recur.value === 'daily' ? 86400 : recur.value === 'weekly' ? 604800 : Number(recurSecs.value) || null
  return { ...e.value, starts_at: fromIso(starts.value) ?? 0, ends_at: fromIso(ends.value), recurrence_secs: rs, labels: labels.value.split(',').map((s) => s.trim()).filter(Boolean), metadata: JSON.parse(meta.value || '{}'), audience: e.value.audience || null, max_participants: e.value.max_participants || null, participation_secs: e.value.participation_secs || null,
    phases: phases.value.map((p) => ({ name: p.name, offset_secs: Number(p.offset_secs), duration_secs: Number(p.duration_secs), ...(p.metadata.trim() ? { metadata: JSON.parse(p.metadata) } : {}) })) }
}
async function save() { try { const b = body(); const r = isNew.value ? await post('/v1/server/live_events', b) : await put(E.value, b); toast.show('saved'); if (isNew.value) router.replace(`/liveops/live_events/${encodeURIComponent(r.id ?? r.live_event?.id)}`); else load() } catch (x: any) { toast.err(x) } }
async function del() { try { await api('DELETE', E.value); toast.show('deleted'); router.push('/liveops/live_events') } catch (x: any) { toast.err(x) } }
function addPhase() { const last = phases.value[phases.value.length - 1]; phases.value.push({ name: `phase ${phases.value.length + 1}`, offset_secs: last ? Number(last.offset_secs) + Number(last.duration_secs) : 0, duration_secs: last ? Number(last.duration_secs) : 3600, metadata: '' }) }
function move(i: number, d: number) { const j = i + d; if (j < 0 || j >= phases.value.length) return; const a = phases.value; [a[i], a[j]] = [a[j], a[i]] }
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
</script>
<template>
  <div class="crumb"><router-link to="/liveops/live_events">Live events</router-link> › <span>{{ isNew ? 'new event' : e?.name || id }}</span><span class="id" v-if="!isNew">{{ id }} · {{ state }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="!isNew" @click="ask('Delete this live event?') && del()">Delete</button><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="e">
    <div class="grid">
      <div><Kv k="Name"><input class="in" v-model="e.name" /></Kv><Kv k="Description"><input class="in" v-model="e.description" /></Kv><Kv k="Enabled"><label><input type="checkbox" v-model="e.enabled" /> scheduled and visible to clients</label></Kv><Kv k="Labels (comma-separated)"><input class="in" v-model="labels" placeholder="rush, weekend" /></Kv></div>
      <div><Kv k="Starts"><input class="in" type="datetime-local" v-model="starts" /></Kv><Kv k="Ends (empty = open-ended / until recurrence stops)"><input class="in" type="datetime-local" v-model="ends" /></Kv>
        <Kv k="Recurrence"><div style="display:flex;gap:8px"><select class="sel" v-model="recur"><option value="none">one-off</option><option value="hourly">every hour</option><option value="daily">every day</option><option value="weekly">every week</option><option value="custom">custom seconds</option></select><input class="in" type="number" min="60" v-if="recur === 'custom'" v-model.number="recurSecs" style="max-width:140px" /></div></Kv>
        <Kv k="Local time"><label><input type="checkbox" v-model="e.local_time" /> schedule in each player's local time zone</label></Kv></div>
      <div><Kv k="Participation"><select class="sel" v-model="e.participation"><option value="implicit">implicit — every eligible player is in</option><option value="opt_in">opt-in — player joins explicitly</option><option value="invite">invite — server grants participation</option></select></Kv><Kv k="Audience"><select class="sel" v-model="e.audience"><option :value="null">everyone</option><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option></select></Kv>
        <Kv k="Max participants / join window (s)"><div style="display:flex;gap:8px"><input class="in" type="number" min="1" v-model.number="e.max_participants" placeholder="unlimited" /><input class="in" type="number" min="0" v-model.number="e.participation_secs" placeholder="whole event" /></div></Kv><Kv k="Template" :v="e.template ?? '—'" /></div>
    </div>
    <div class="tabs" style="margin-top:12px"><a :class="{ on: tab === 'phases' }" @click="tab = 'phases'">Phases ({{ phases.length }})</a><a v-if="!isNew" :class="{ on: tab === 'participants' }" @click="tab = 'participants'">Participants ({{ participants.length }})</a></div>
    <template v-if="tab === 'phases'">
      <h3>Phases <small>(relative to each occurrence start · total {{ fmt(total) }})</small> <button class="btn s p" style="margin-left:8px" @click="addPhase">Add phase</button></h3>
      <div class="bar" style="height:10px;margin:6px 0 10px;position:relative" v-if="total"><i v-for="(p, i) in phases" :key="i" :style="{ position: 'absolute', left: (p.offset_secs / total) * 100 + '%', width: (p.duration_secs / total) * 100 + '%', opacity: 0.45 + 0.5 * ((i % 3) / 2) }" :title="p.name"></i></div>
      <table><thead><tr><th style="width:40px">#</th><th>Name</th><th style="width:150px">Offset (s)</th><th style="width:150px">Duration (s)</th><th style="width:150px">Window</th><th>Metadata (JSON)</th><th style="width:110px"></th></tr></thead><tbody>
        <tr v-for="(p, i) in phases" :key="i"><td class="dim">{{ i + 1 }}</td><td><input class="in" style="min-width:0;width:100%" v-model="p.name" /></td><td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="p.offset_secs" /></td><td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="p.duration_secs" /></td><td class="dim">+{{ fmt(Number(p.offset_secs)) }} → +{{ fmt(Number(p.offset_secs) + Number(p.duration_secs)) }}</td><td><input class="in m" style="min-width:0;width:100%" v-model="p.metadata" placeholder="{}" /></td><td><button class="btn s" @click="move(i, -1)">↑</button> <button class="btn s" @click="move(i, 1)">↓</button> <button class="btn s" @click="phases.splice(i, 1)">×</button></td></tr>
        <tr v-if="!phases.length"><td colspan="7" class="empty">No phases — the event is a single window</td></tr></tbody></table>
      <h3>Metadata (JSON)</h3><textarea class="in m" v-model="meta" style="min-height:90px"></textarea>
    </template>
    <template v-else>
      <DataTable :cols="[{ key: 'account_id', mono: true }, { key: 'joined_at', fmt: (r) => ts(r.joined_at ?? r.entered_at), dim: true }, { key: 'occurrence', fmt: (r) => r.occurrence != null ? ts(r.occurrence) : '', dim: true }, { key: 'state', fmt: (r) => r.state ?? r.kind ?? '' }]" :rows="participants" empty="No participants recorded (implicit events track nobody until they act)" pick @pick="(r) => router.push(`/players/accounts/${r.account_id}`)" />
    </template>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="e" />
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
