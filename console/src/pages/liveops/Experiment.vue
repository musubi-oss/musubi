<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => String(route.params.id)); const E = computed(() => `/v1/server/experiments/${id.value}`)
const e = ref<any>(null); const err = ref(''); const audiences = ref<string[]>([]); const labels = ref(''); const winner = ref('')
interface V { name: string; weight: number; flags: string }
const variants = ref<V[]>([])
function toIso(v: any) { return v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '' }
function fromIso(s: string) { return s ? Math.floor(new Date(s).getTime() / 1000) : null }
const starts = ref(''); const ends = ref('')
async function load() {
  err.value = ''
  try {
    const r = await get(E.value); e.value = r.experiment ?? r
    variants.value = (e.value.variants ?? []).map((v: any) => ({ name: v.name, weight: v.weight ?? 50, flags: JSON.stringify(v.flags ?? {}, null, 0) }))
    labels.value = (e.value.labels ?? []).join(', '); starts.value = toIso(e.value.starts_at); ends.value = toIso(e.value.ends_at)
  } catch (x: any) { err.value = x.message }
  try { audiences.value = ((await get('/v1/server/audiences')).audiences ?? []).map((a: any) => a.name) } catch {}
}
onMounted(load)
const total = computed(() => variants.value.reduce((s, v) => s + Number(v.weight || 0), 0))
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
function save() {
  return act(() => api('PUT', E.value, { name: e.value.name, description: e.value.description ?? '', audience: e.value.audience || null, exclusion_group: e.value.exclusion_group || null, rollout_pct: Number(e.value.rollout_pct ?? 100), labels: labels.value.split(',').map((s) => s.trim()).filter(Boolean), starts_at: fromIso(starts.value), ends_at: fromIso(ends.value), variants: variants.value.map((v) => ({ name: v.name, weight: Number(v.weight), flags: JSON.parse(v.flags || '{}') })), metadata: e.value.metadata ?? {} }), 'saved')
}
</script>
<template>
  <div class="crumb"><router-link to="/liveops/experiments">Experiments</router-link> › <span>{{ e?.name || id }}</span><span class="id">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="e">
      <button class="btn s" v-if="e.status !== 'running' && e.status !== 'ended'" @click="act(() => post(`${E}/start`), 'started')">Start</button>
      <button class="btn s" v-if="e.status === 'running'" @click="act(() => post(`${E}/pause`), 'paused')">Pause</button>
      <template v-if="e.status !== 'ended'"><select class="sel" style="min-width:140px" v-model="winner"><option value="">no winner</option><option v-for="v in variants" :key="v.name" :value="v.name">winner: {{ v.name }}</option></select><button class="btn s d" @click="ask('End this experiment?') && act(() => post(`${E}/end`, { winner: winner || undefined }), 'ended')">End</button></template>
      <button class="btn s d" @click="ask('Delete experiment?') && act(async () => { await api('DELETE', E); router.push('/liveops/experiments') }, 'deleted')">Delete</button>
      <button class="btn s p" @click="save">Save</button>
    </span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="e">
    <div class="grid">
      <div><Kv k="Status"><span :class="{ ok: e.status === 'running' }">{{ e.status }}</span></Kv><Kv k="Name"><input class="in" v-model="e.name" /></Kv><Kv k="Description"><input class="in" v-model="e.description" /></Kv></div>
      <div><Kv k="Audience"><select class="sel" v-model="e.audience"><option :value="null">everyone</option><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option></select></Kv><Kv k="Rollout % of eligible players"><input class="in" type="number" min="0" max="100" v-model.number="e.rollout_pct" /></Kv><Kv k="Exclusion group (one experiment per player per group)"><input class="in" v-model="e.exclusion_group" /></Kv></div>
      <div><Kv k="Starts"><input class="in" type="datetime-local" v-model="starts" /></Kv><Kv k="Ends"><input class="in" type="datetime-local" v-model="ends" /></Kv><Kv k="Labels"><input class="in" v-model="labels" /></Kv></div>
    </div>
    <h3>Variants <small>({{ variants.length }}, weights sum {{ total }})</small> <button class="btn s p" style="margin-left:8px" @click="variants.push({ name: `v${variants.length + 1}`, weight: 50, flags: '{}' })">Add variant</button></h3>
    <table><thead><tr><th style="width:200px">Name</th><th style="width:110px">Weight</th><th style="width:120px">Share</th><th>Flag overrides (JSON: flag → value)</th><th style="width:60px"></th></tr></thead><tbody>
      <tr v-for="(v, i) in variants" :key="i">
        <td><input class="in" style="min-width:0;width:100%" v-model="v.name" /></td>
        <td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="v.weight" /></td>
        <td><div class="bar"><i :style="{ width: (total ? (v.weight / total) * 100 : 0) + '%' }"></i></div><span class="dim">{{ total ? Math.round((v.weight / total) * 100) : 0 }}%</span></td>
        <td><input class="in m" style="min-width:0;width:100%" v-model="v.flags" /></td>
        <td><button class="btn s d" @click="variants.splice(i, 1)">×</button></td>
      </tr></tbody></table>
    <h3>Raw</h3><JsonView :value="e" />
  </div>
</template>
