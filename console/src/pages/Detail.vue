<script setup lang="ts">
// Generic detail page for any catalog resource with an `item` endpoint: the record as a key/value
// sheet (ids cross-linked through refs.ts), raw JSON, edit/save/delete, the row actions, and the
// related lists the catalog declares in `detail.tabs` (paths with {id}/{field} placeholders).
import { ref, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, qs } from '../api/client'
import type { Res, Act } from '../catalog'
import { refFor } from '../refs'
import DataTable, { type Col } from '../components/DataTable.vue'
import Drawer from '../components/Drawer.vue'
import JsonView from '../components/JsonView.vue'
import Kv from '../components/Kv.vue'
import { toast } from '../stores/toast'
const props = defineProps<{ res: Res }>()
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id))
const item = ref<any>(null); const err = ref(''); const busy = ref(false); const editing = ref(false); const text = ref('')
const pending = ref<{ act: Act; text: string } | null>(null)
const tabs = computed(() => [{ id: 'fields', title: 'fields' }, ...(props.res.detail?.tabs ?? []), { id: 'json', title: 'json' }])
const tab = ref(String(route.query.tab ?? 'fields'))
const tabRows = ref<any[]>([]); const tabErr = ref(''); const tabBusy = ref(false)
const key = computed(() => props.res.key ?? 'id')
function fill(path: string, row: any) { return path.replace(/\{([^}]+)\}/g, (_, k) => encodeURIComponent(k === 'id' ? id.value : (row?.[k] ?? id.value))) }
const itemPath = computed(() => (props.res.item ? fill(props.res.item, { ...(item.value ?? {}), [key.value]: id.value }) : null))
// Resources without a GET-by-id route are looked up in their list: the list already carries the
// full record for those, and every row still gets a page with tabs, actions and cross-links.
async function fromList() {
  const d = await get(props.res.list + qs({ limit: 500 }))
  const list = Array.isArray(d) ? d : props.res.rowsKey ? d?.[props.res.rowsKey] : Object.values(d ?? {}).find(Array.isArray)
  const row = (Array.isArray(list) ? list : []).find((r: any) => String(r?.[key.value] ?? r?.id ?? r?.name) === id.value)
  if (!row) throw new Error(`${props.res.title.replace(/s$/, '')} ${id.value} is not in the list any more`)
  return row
}
async function load() {
  busy.value = true; err.value = ''
  try {
    const r = itemPath.value ? await get(itemPath.value) : await fromList()
    item.value = r && typeof r === 'object' && !Array.isArray(r) && Object.keys(r).length === 1 && typeof Object.values(r)[0] === 'object' ? Object.values(r)[0] : r
    text.value = JSON.stringify(item.value, null, 2)
  } catch (e: any) { err.value = e.message; item.value = null }
  busy.value = false
}
async function loadTab(t: string) {
  const def = props.res.detail?.tabs?.find((x) => x.id === t); if (!def) return
  tabBusy.value = true; tabErr.value = ''
  try {
    const d = await get(fill(def.list, item.value) + (def.list.includes('?') ? '' : qs({ limit: 100 })))
    const list = Array.isArray(d) ? d : def.rowsKey ? d?.[def.rowsKey] : Object.values(d ?? {}).find(Array.isArray)
    tabRows.value = Array.isArray(list) ? list : []
  } catch (e: any) { tabErr.value = e.message; tabRows.value = [] }
  tabBusy.value = false
}
const tabCols = computed<Col[]>(() => {
  const def = props.res.detail?.tabs?.find((x) => x.id === tab.value)
  const keys = def?.cols ?? (tabRows.value[0] ? Object.keys(tabRows.value[0]).slice(0, 7) : ['id'])
  return keys.map((k) => ({ key: k, label: k.replace(/_/g, ' '), mono: /(^|_)(id|sha256|hash|address|code)$/.test(k) }))
})
watch(tab, (t) => { router.replace({ query: { ...route.query, tab: t === 'fields' ? undefined : t } }); loadTab(t) })
watch(id, () => { load().then(() => loadTab(tab.value)) }, { immediate: true })
const fields = computed(() => Object.entries(item.value ?? {}).filter(([, v]) => typeof v !== 'object' || v === null))
const nested = computed(() => Object.entries(item.value ?? {}).filter(([, v]) => typeof v === 'object' && v !== null))
function fmtVal(k: string, v: any) {
  if (v === null || v === undefined || v === '') return '—'
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (typeof v === 'number' && /(_at|At|_until|_ts|_since)$/.test(k) && v > 1e9) return new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 19) + ' UTC'
  return String(v)
}
const title = computed(() => item.value?.name ?? item.value?.title ?? item.value?.username ?? item.value?.display_name ?? id.value)
const listPath = computed(() => `/${String(route.path.split('/')[1])}/${props.res.id}`)
async function save() {
  try { await api(props.res.method ?? 'PUT', itemPath.value!, JSON.parse(text.value)); toast.show('saved'); editing.value = false; load() } catch (e: any) { toast.err(e) }
}
async function remove() {
  if (!confirm(`Delete ${props.res.title.replace(/s$/, '').toLowerCase()} ${id.value}?`)) return
  try { await api('DELETE', itemPath.value!); toast.show('deleted'); router.push(listPath.value) } catch (e: any) { toast.err(e) }
}
function visible(acts: Act[] | undefined) { return (acts ?? []).filter((a) => !a.when || a.when(item.value)) }
async function run(act: Act) {
  if (act.body && !pending.value) { pending.value = { act, text: JSON.stringify(act.body, null, 2) }; return }
  if (act.confirm && !window.confirm(act.confirm)) return
  try {
    const body = pending.value ? JSON.parse(pending.value.text) : undefined
    await api(act.method, fill(act.path, { ...(item.value ?? {}), ...(body ?? {}) }), body)
    toast.show(act.label + ': ok'); pending.value = null; load()
  } catch (e: any) { toast.err(e) }
}
function copyJson() { navigator.clipboard?.writeText(text.value); toast.show('copied') }
</script>
<template>
  <div class="crumb"><router-link :to="listPath">{{ res.title }}</router-link> › <span>{{ title }}</span><span class="id" v-if="title !== id">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="item">
      <button v-for="a in visible(res.actions)" :key="a.label" class="btn s" :class="{ d: a.danger }" @click="run(a)">{{ a.label }}</button>
      <button class="btn s" v-if="res.item && res.method !== 'POST'" @click="editing = !editing; text = JSON.stringify(item, null, 2)">{{ editing ? 'Cancel' : 'Edit' }}</button>
      <button class="btn s d" v-if="res.item && res.method !== 'POST'" @click="remove">Delete</button>
    </span></div>
  <p class="err" v-if="err">{{ err }} <router-link :to="listPath">← back to {{ res.title.toLowerCase() }}</router-link></p>
  <div class="card" v-else-if="item">
    <div class="tabs"><a v-for="t in tabs" :key="t.id" :class="{ on: tab === t.id }" @click="tab = t.id">{{ t.title }}</a></div>
    <template v-if="editing">
      <textarea class="in" v-model="text" style="min-height:420px"></textarea>
      <div class="form"><div class="row"><button class="btn" @click="editing = false">Cancel</button><button class="btn p" @click="save">Save</button></div></div>
    </template>
    <template v-else-if="tab === 'fields'">
      <div class="grid" style="grid-template-columns:repeat(3,1fr)">
        <div v-for="[k, v] in fields" :key="k" style="border:0;margin:0"><Kv :k="k.replace(/_/g, ' ')" :v="fmtVal(k, v)" :mono="/(^|_)(id|sha256|hash|address|code)$/.test(k)" :to="refFor(k, v, item)" copy /></div>
      </div>
      <div v-for="[k, v] in nested" :key="k" style="margin-top:14px"><div class="k" style="margin-bottom:6px">{{ k.replace(/_/g, ' ') }}</div><JsonView :value="v" /></div>
    </template>
    <template v-else-if="tab === 'json'">
      <div class="tools" style="margin-bottom:8px"><button class="btn s" @click="copyJson">Copy</button></div>
      <JsonView :value="item" />
    </template>
    <template v-else>
      <p class="err" v-if="tabErr">{{ tabErr }}</p>
      <DataTable :cols="tabCols" :rows="tabRows" :loading="tabBusy" :empty="`No ${tabs.find((t) => t.id === tab)?.title ?? 'rows'} yet`" />
    </template>
  </div>
  <div class="card" v-else-if="busy"><p class="note">Loading…</p></div>
  <Drawer v-if="pending" :title="pending.act.label" @close="pending = null">
    <p class="note m">{{ pending.act.method }} {{ fill(pending.act.path, item ?? {}) }}</p>
    <textarea class="in" v-model="pending.text" style="min-height:260px"></textarea>
    <div class="form"><div class="row"><button class="btn" @click="pending = null">Cancel</button><button class="btn p" :class="{ d: pending.act.danger }" @click="run(pending.act)">Send</button></div></div>
  </Drawer>
</template>
