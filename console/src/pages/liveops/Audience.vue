<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const name = computed(() => String(route.params.name)); const U = computed(() => `/v1/server/audiences/${encodeURIComponent(name.value)}`)
const a = ref<any>(null); const err = ref(''); const members = ref<any>(null); const tab = ref('rules')
const r = ref<any>({}); const lists = ref<Record<string, string>>({}); const nested = ref({ any: '[]', all: '[]', not: '' }); const actions = ref('{\n  "on_enter": [],\n  "on_exit": []\n}'); const tracked = ref(false)
const listKeys = ['platform', 'lang', 'location', 'age_bracket', 'include', 'exclude']
const numKeys: [string, string][] = [['min_level', 'Min level'], ['max_level', 'Max level'], ['min_account_age_days', 'Min account age (days)'], ['max_account_age_days', 'Max account age (days)'], ['active_within_days', 'Active within (days)'], ['inactive_days', 'Inactive for (days)'], ['bucket_min', 'Bucket min (0..99)'], ['bucket_max', 'Bucket max (0..99)']]
function toIso(v: any) { return v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '' }
function fromIso(s: string) { return s ? Math.floor(new Date(s).getTime() / 1000) : null }
const created = ref({ after: '', before: '' })
async function load() {
  err.value = ''
  try {
    const x = await get(U.value); a.value = x.audience ?? x
    const rules = a.value.rules ?? {}
    r.value = { ...rules }
    for (const k of listKeys) lists.value[k] = (rules[k] ?? []).join(', ')
    nested.value = { any: JSON.stringify(rules.any ?? [], null, 2), all: JSON.stringify(rules.all ?? [], null, 2), not: rules.not ? JSON.stringify(rules.not, null, 2) : '' }
    created.value = { after: toIso(rules.created_after), before: toIso(rules.created_before) }
    tracked.value = !!a.value.tracked; actions.value = JSON.stringify(a.value.actions ?? { on_enter: [], on_exit: [] }, null, 2)
  } catch (e: any) { err.value = e.message }
  try { members.value = await get(`${U.value}/members`) } catch (e: any) { members.value = e.status === 404 ? null : { error: e.message } }
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (e: any) { toast.err(e) } }
function body() {
  const rules: any = {}
  for (const [k] of numKeys) if (r.value[k] !== null && r.value[k] !== '' && r.value[k] !== undefined) rules[k] = Number(r.value[k])
  for (const k of listKeys) { const v = lists.value[k].split(',').map((s) => s.trim()).filter(Boolean); if (v.length) rules[k] = v }
  if (r.value.bucket_salt) rules.bucket_salt = r.value.bucket_salt
  if (r.value.track) rules.track = r.value.track
  if (r.value.metadata) rules.metadata = r.value.metadata
  const any = JSON.parse(nested.value.any || '[]'); if (any.length) rules.any = any
  const all = JSON.parse(nested.value.all || '[]'); if (all.length) rules.all = all
  if (nested.value.not.trim()) rules.not = JSON.parse(nested.value.not)
  const ca = fromIso(created.value.after), cb = fromIso(created.value.before); if (ca) rules.created_after = ca; if (cb) rules.created_before = cb
  return { rules, tracked: tracked.value, actions: JSON.parse(actions.value || '{}') }
}
const save = () => act(() => api('PUT', U.value, body()), 'audience saved')
const mrows = computed(() => { const m = members.value; if (!m || m.error) return []; const v = m.members ?? Object.values(m).find(Array.isArray); return Array.isArray(v) ? v.map((x: any) => typeof x === 'string' ? { account_id: x } : x) : [] })
</script>
<template>
  <div class="crumb"><router-link to="/liveops/audiences">Audiences</router-link> › <span class="m">{{ name }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="act(() => post(`${U}/refresh`), 'membership refreshed')">Refresh members</button><button class="btn s d" @click="ask(`Delete audience ${name}?`) && act(async () => { await api('DELETE', U); router.push('/liveops/audiences') }, 'deleted')">Delete</button><button class="btn s p" :disabled="!a" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="a">
    <div class="tabs"><a :class="{ on: tab === 'rules' }" @click="tab = 'rules'">rules</a><a :class="{ on: tab === 'members' }" @click="tab = 'members'">members ({{ members?.total ?? mrows.length }})</a><a :class="{ on: tab === 'raw' }" @click="tab = 'raw'">raw</a></div>
    <template v-if="tab === 'rules'">
      <div class="grid">
        <div><Kv k="Tracked"><label><input type="checkbox" v-model="tracked" /> keep a member table and run actions on enter / exit</label></Kv><Kv k="Platform (comma-separated)"><input class="in" v-model="lists.platform" placeholder="ios, android, pc" /></Kv><Kv k="Language"><input class="in" v-model="lists.lang" placeholder="en, de" /></Kv><Kv k="Location (GeoIP country / region)"><input class="in" v-model="lists.location" placeholder="DE, FR" /></Kv><Kv k="Age bracket"><input class="in" v-model="lists.age_bracket" placeholder="18-24, 25-34" /></Kv></div>
        <div><Kv v-for="[k, l] in numKeys" :key="k" :k="l"><input class="in" type="number" v-model="r[k]" placeholder="any" /></Kv><Kv k="Bucket salt"><input class="in" v-model="r.bucket_salt" placeholder="default salt" /></Kv></div>
        <div><Kv k="Created after"><input class="in" type="datetime-local" v-model="created.after" /></Kv><Kv k="Created before"><input class="in" type="datetime-local" v-model="created.before" /></Kv><Kv k="Always include (account ids)"><textarea class="in" v-model="lists.include" style="min-height:60px"></textarea></Kv><Kv k="Always exclude (account ids)"><textarea class="in" v-model="lists.exclude" style="min-height:60px"></textarea></Kv></div>
      </div>
      <h3>Rule algebra <small>(nested rule objects with the same fields)</small></h3>
      <div class="grid">
        <div><Kv k="any — at least one matches"><textarea class="in m" v-model="nested.any" style="min-height:120px"></textarea></Kv></div>
        <div><Kv k="all — every one matches"><textarea class="in m" v-model="nested.all" style="min-height:120px"></textarea></Kv></div>
        <div><Kv k="not — must not match"><textarea class="in m" v-model="nested.not" style="min-height:120px" placeholder="{ }"></textarea></Kv></div>
      </div>
      <h3>Actions on change <small>(tracked only: reward_table, flag, webhook, …)</small></h3>
      <textarea class="in m" v-model="actions" style="min-height:120px"></textarea>
    </template>
    <template v-else-if="tab === 'members'">
      <p class="dim" v-if="!a.tracked">Not tracked — membership is evaluated per request, no member table. Enable “tracked” and save to materialise it.</p>
      <p class="err" v-else-if="members?.error">{{ members.error }}</p>
      <DataTable v-else :cols="[{ key: 'account_id', mono: true }, { key: 'kind' }, { key: 'entered_at', fmt: (x) => x.entered_at ?? x.since ?? '—', dim: true }]" :rows="mrows" pick @pick="(x) => router.push(`/players/accounts/${x.account_id ?? x.id}`)" />
    </template>
    <template v-else><JsonView :value="a" /></template>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
