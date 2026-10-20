<script setup lang="ts">
// Generic resource page: list table from `res.list`, item drawer (raw JSON + edit), create via
// template, delete. Non-list endpoints render as a key/value sheet of the JSON object.
import { ref, onMounted, computed } from 'vue'
import { api, get, qs } from '../api/client'
import type { Res, Act } from '../catalog'
import { useRouter } from 'vue-router'
import DataTable, { type Col } from '../components/DataTable.vue'
import Drawer from '../components/Drawer.vue'
import JsonView from '../components/JsonView.vue'
import Kv from '../components/Kv.vue'
import { toast } from '../stores/toast'
const props = defineProps<{ res: Res }>()
const router = useRouter()
const pending = ref<{ act: Act; row: any; text: string } | null>(null)
const data = ref<any>(null); const rows = ref<any[]>([]); const err = ref(''); const busy = ref(false)
const q = ref(''); const cursor = ref<string | null>(null); const stack = ref<string[]>([])
const open = ref<any>(null); const edit = ref(''); const creating = ref(false); const text = ref('')
const cols = computed<Col[]>(() => {
  const keys = props.res.cols ?? (rows.value[0] ? Object.keys(rows.value[0]).slice(0, 7) : ['id'])
  return keys.map((k) => ({ key: k, label: k.replace(/_/g, ' '), mono: /(^|_)(id|sha256|hash|address|code)$/.test(k) }))
})
const isList = computed(() => Array.isArray(rows.value) && (rows.value.length > 0 || props.res.rowsKey !== undefined || Array.isArray(data.value)))
async function load(c: string | null = null) {
  busy.value = true; err.value = ''
  try {
    const d = await get(props.res.list + qs({ q: q.value, cursor: c, limit: 50 }))
    data.value = d
    const list = Array.isArray(d) ? d : props.res.rowsKey ? d?.[props.res.rowsKey] : Object.values(d ?? {}).find(Array.isArray)
    rows.value = Array.isArray(list) ? list : []
    cursor.value = d?.next_cursor ?? d?.cursor ?? null
  } catch (e: any) { err.value = e.message; rows.value = []; data.value = null }
  busy.value = false
}
function next() { if (cursor.value) { stack.value.push(cursor.value); load(cursor.value) } }
function prev() { stack.value.pop(); load(stack.value[stack.value.length - 1] ?? null) }
function itemPath(row: any) {
  if (!props.res.item) return null
  return props.res.item.replace(/\{([^}]+)\}/g, (_, k) => encodeURIComponent(row[k] ?? row[props.res.key ?? 'id'] ?? row.id ?? row.name))
}
async function pick(row: any) {
  const p = itemPath(row)
  open.value = row; edit.value = JSON.stringify(row, null, 2)
  if (p) { try { open.value = await get(p); edit.value = JSON.stringify(open.value, null, 2) } catch (e: any) { toast.err(e) } }
}
async function save() {
  try {
    const body = JSON.parse(edit.value)
    const p = itemPath(open.value)
    if (!p) throw new Error('read-only resource')
    await api(props.res.method ?? 'PUT', p, body); toast.show('saved'); open.value = null; load()
  } catch (e: any) { toast.err(e) }
}
async function remove() {
  const p = itemPath(open.value); if (!p || !confirm('Delete this record?')) return
  try { await api('DELETE', p); toast.show('deleted'); open.value = null; load() } catch (e: any) { toast.err(e) }
}
function startCreate() { creating.value = true; text.value = JSON.stringify({ ...(props.res.key ? { [props.res.key]: '' } : {}), ...(props.res.template ?? {}) }, null, 2) }
async function create() {
  try {
    const body = JSON.parse(text.value)
    if (props.res.method === 'PUT' && props.res.item) { const k = props.res.key ?? 'id'; const p = itemPath(body); const b = { ...body }; delete b[k]; await api('PUT', p!, b) }
    else await api('POST', props.res.list, body)
    toast.show('created'); creating.value = false; load()
  } catch (e: any) { toast.err(e) }
}
function fill(path: string, row: any) { return path.replace(/\{([^}]+)\}/g, (_, k) => encodeURIComponent(row?.[k] ?? '')) }
async function run(act: Act, row: any) {
  if (act.body && !pending.value) { pending.value = { act, row, text: JSON.stringify(act.body, null, 2) }; return }
  if (act.confirm && !window.confirm(act.confirm)) return
  try {
    const body = pending.value ? JSON.parse(pending.value.text) : undefined
    await api(act.method, fill(act.path, { ...(row ?? {}), ...(body ?? {}) }), body)
    toast.show(act.label + ': ok'); pending.value = null; open.value = null; load()
  } catch (e: any) { toast.err(e) }
}
function visible(acts: Act[] | undefined, row: any) { return (acts ?? []).filter((a) => !a.when || a.when(row)) }
onMounted(() => { if (props.res.custom !== 'zanshin') load() })
</script>
<template>
  <div class="card">
    <h2>{{ res.title }} <small v-if="isList">({{ rows.length }})</small></h2>
    <div class="tools" v-if="isList && res.custom !== 'zanshin'">
      <input class="in" v-model="q" placeholder="search" @keyup.enter="load()" />
      <button class="btn" @click="load()" :disabled="busy">Refresh</button>
      <span style="margin-left:auto;display:flex;gap:8px">
        <button v-for="a in res.pageActions ?? []" :key="a.label" class="btn" :class="{ d: a.danger }" @click="run(a, null)">{{ a.label }}</button>
        <button class="btn p" v-if="res.template || (!res.method && res.item)" @click="startCreate">Create</button>
      </span>
    </div>
    <p class="err" v-if="err">{{ err }}</p>
    <template v-if="res.custom === 'zanshin'">
      <p>Zanshin is Musubi's LiveOps companion and runs as a <b>separate service</b>: it is never started by this cluster and nothing here depends on it. Musubi exposes two integration points:</p>
      <div class="grid" style="margin-top:14px">
        <div><Kv k="Change webhook (Zanshin → Musubi)" v="POST /v1/server/liveops/changed" mono copy /><div class="note">Point Zanshin's flag / live-event change webhook here (server key). Cached LiveOps answers are dropped on every node and WASM modules receive <code>hook_liveops_changed</code>.</div></div>
        <div><Kv k="Outbound webhooks (Musubi → Zanshin)" v="Settings → Webhooks" /><div class="note">Subscribe Zanshin to account, economy, match and fleet events to drive its own campaigns.</div></div>
        <div><Kv k="This section" v="Musubi-native schedule" /><div class="note">Live events, flags, experiments, remote config, audiences, news, offers, tournaments and economy changesets on this page are Musubi's own primitives and work without Zanshin.</div></div>
      </div>
    </template>
    <template v-else-if="isList">
      <DataTable :cols="cols" :rows="rows" pick @pick="pick">
        <template #actions="{ row }" v-if="res.actions || res.link">
          <span style="display:flex;gap:6px;justify-content:flex-end">
            <button v-if="res.link && res.link(row)" class="btn s" @click="router.push(res.link(row)!)">Open</button>
            <button v-for="a in visible(res.actions, row)" :key="a.label" class="btn s" :class="{ d: a.danger }" @click="run(a, row)">{{ a.label }}</button>
          </span>
        </template>
      </DataTable>
      <div class="pager" v-if="cursor || stack.length"><button class="btn s" :disabled="!stack.length" @click="prev">Prev</button><button class="btn s" :disabled="!cursor" @click="next">Next</button></div>
    </template>
    <template v-else-if="data && typeof data === 'object'">
      <div class="grid" style="grid-template-columns:repeat(3,1fr)">
        <div v-for="(v, k) in data" :key="String(k)" style="border:0;margin:0"><Kv :k="String(k).replace(/_/g, ' ')" :v="typeof v === 'object' && v !== null ? undefined : v" :mono="typeof v === 'string'">
          <JsonView v-if="typeof v === 'object' && v !== null" :value="v" /><template v-else>{{ v ?? '—' }}</template></Kv></div>
      </div>
      <div class="tools"><button class="btn" @click="load()">Refresh</button><button v-for="a in res.pageActions ?? []" :key="a.label" class="btn" :class="{ d: a.danger }" @click="run(a, data)">{{ a.label }}</button></div>
    </template>
    <div v-else-if="!err && !busy" class="empty">No Data</div>
  </div>
  <Drawer v-if="open" :title="res.title.replace(/s$/, '')" @close="open = null">
    <template v-if="!res.item"><JsonView :value="open" />
      <div class="form" v-if="res.actions?.length"><div class="row"><button v-for="a in visible(res.actions, open)" :key="a.label" class="btn" :class="{ d: a.danger }" @click="run(a, open)">{{ a.label }}</button></div></div></template>
    <template v-else>
      <textarea class="in" v-model="edit" style="min-height:360px"></textarea>
      <div class="form"><div class="row"><button v-for="a in visible(res.actions, open)" :key="a.label" class="btn" :class="{ d: a.danger }" @click="run(a, open)">{{ a.label }}</button><button class="btn d" @click="remove">Delete</button><button class="btn" @click="open = null">Close</button><button class="btn p" @click="save">Save</button></div></div>
    </template>
  </Drawer>
  <Drawer v-if="creating" :title="'New ' + res.title.replace(/s$/, '').toLowerCase()" @close="creating = false">
    <textarea class="in" v-model="text" style="min-height:300px"></textarea>
    <div class="form"><div class="row"><button class="btn" @click="creating = false">Cancel</button><button class="btn p" @click="create">Create</button></div></div>
  </Drawer>
  <Drawer v-if="pending" :title="pending.act.label" @close="pending = null">
    <p class="note m">{{ pending.act.method }} {{ fill(pending.act.path, pending.row ?? {}) }}</p>
    <textarea class="in" v-model="pending.text" style="min-height:260px"></textarea>
    <div class="form"><div class="row"><button class="btn" @click="pending = null">Cancel</button><button class="btn p" :class="{ d: pending.act.danger }" @click="run(pending.act, pending.row)">Send</button></div></div>
  </Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
