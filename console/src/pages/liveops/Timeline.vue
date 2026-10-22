<script setup lang="ts">
import { useQueryRef } from '../../composables/query'
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { get, qs } from '../../api/client'
const router = useRouter()
const days = ref(7); const back = ref(1); const kinds = ref<string[]>([]); const label = useQueryRef('label'); const d = ref<any>(null); const err = ref(''); const state = useQueryRef('state', 'all')
const allKinds = ['live_event', 'flag', 'experiment', 'news', 'offer', 'tournament', 'changeset']
async function load() {
  err.value = ''
  const now = Math.floor(Date.now() / 1000)
  try { d.value = await get('/v1/server/liveops/timeline' + qs({ from: now - back.value * 86400, to: now + days.value * 86400, kinds: kinds.value.join(',') || undefined, label: label.value || undefined })) }
  catch (e: any) { err.value = e.message }
}
onMounted(load)
function ts(v: any) { if (!v) return '—'; const t = typeof v === 'number' ? new Date(v * 1000) : new Date(v); return t.toISOString().replace('T', ' ').slice(0, 16) }
function dur(a: number, b: number) { if (!a || !b) return '—'; const m = Math.round((b - a) / 60); return m < 60 ? `${m} min` : m < 1440 ? `${(m / 60).toFixed(1)} h` : `${(m / 1440).toFixed(1)} d` }
const rows = computed(() => (d.value?.entries ?? []).filter((e: any) => state.value === 'all' || e.state === state.value))
const pathOf: Record<string, string> = { live_event: 'live_events', flag: 'flags', experiment: 'experiments', news: 'news', offer: 'offers', tournament: 'tournaments', changeset: 'changesets' }
function open(e: any) { const p = pathOf[e.kind]; if (p && e.id) router.push(`/liveops/${p}`) }
function toggle(k: string) { kinds.value = kinds.value.includes(k) ? kinds.value.filter((x) => x !== k) : [...kinds.value, k]; load() }
const pg1 = usePager(() => rows)
</script>
<template>
  <div class="stats" v-if="d">
    <div class="stat" v-for="(v, k) in d.summary?.by_kind ?? {}" :key="String(k)"><div class="k">{{ String(k).replace('_', ' ') }}</div><div class="v">{{ v.active }}<small>active · {{ v.upcoming }} upcoming · {{ v.past }} past</small></div></div>
  </div>
  <div class="card">
    <h2>Timeline <small v-if="d">({{ rows.length }} of {{ d.total }})</small></h2>
    <div class="tools">
      <select class="sel" v-model.number="back" @change="load"><option :value="1">1 day back</option><option :value="7">7 days back</option><option :value="30">30 days back</option></select>
      <select class="sel" v-model.number="days" @change="load"><option :value="1">next 24 h</option><option :value="7">next 7 days</option><option :value="30">next 30 days</option><option :value="90">next 90 days</option></select>
      <select class="sel" v-model="state"><option value="all">all states</option><option value="active">active</option><option value="upcoming">upcoming</option><option value="past">past</option></select>
      <input class="in" v-model="label" placeholder="label" @keyup.enter="load" />
      <span style="display:flex;gap:4px;flex-wrap:wrap"><button v-for="k in allKinds" :key="k" class="btn s" :class="{ p: kinds.includes(k) }" @click="toggle(k)">{{ k.replace('_', ' ') }}</button></span>
      <button class="btn" @click="load">Refresh</button>
    </div>
    <p class="err" v-if="err">{{ err }}</p>
    <h3 style="margin-top:0" v-if="d?.summary?.active_now?.length">Active now</h3>
    <div v-if="d?.summary?.active_now?.length" style="margin-bottom:14px"><span v-for="a in d.summary.active_now" :key="a.id" class="pill" style="margin:0 6px 6px 0">{{ a.kind.replace('_', ' ') }} · {{ a.name || a.id }}</span></div>
    <table><thead><tr><th style="width:150px">Starts</th><th style="width:150px">Ends</th><th style="width:80px">Length</th><th style="width:110px">Kind</th><th>Name</th><th style="width:90px">State</th><th>Labels</th></tr></thead><tbody>
      <tr v-for="(e, i) in pg1.rows" :key="e.id + i" class="row" @click="open(e)">
        <td class="dim">{{ ts(e.starts_at) }}</td><td class="dim">{{ e.ends_at ? ts(e.ends_at) : 'open' }}</td><td class="dim">{{ dur(e.starts_at, e.ends_at) }}</td>
        <td>{{ e.kind.replace('_', ' ') }}</td><td>{{ e.name || e.id }}</td>
        <td :class="{ ok: e.state === 'active', dim: e.state !== 'active' }">{{ e.state }}</td><td class="dim">{{ (e.labels ?? []).join(', ') || '—' }}</td>
      </tr>
      <tr v-if="!pg1.total"><td colspan="7" class="empty">No Data</td></tr></tbody></table><Pager :p="pg1" />
  </div>
</template>
