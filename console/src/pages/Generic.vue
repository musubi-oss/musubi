<script setup lang="ts">
// Generic resource page: list table from `res.list`, item drawer (raw JSON + edit), create via
// template, delete. Non-list endpoints render as a key/value sheet of the JSON object.
import { ref, onMounted, computed } from 'vue'
import { api, get, qs } from '../api/client'
import type { Res } from '../catalog'
import DataTable, { type Col } from '../components/DataTable.vue'
import Drawer from '../components/Drawer.vue'
import JsonView from '../components/JsonView.vue'
import Kv from '../components/Kv.vue'
import { toast } from '../stores/toast'
const props = defineProps<{ res: Res }>()
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
onMounted(load)
</script>
<template>
  <div class="card">
    <h2>{{ res.title }} <small v-if="isList">({{ rows.length }})</small></h2>
    <div class="tools" v-if="isList">
      <input class="in" v-model="q" placeholder="search" @keyup.enter="load()" />
      <button class="btn" @click="load()" :disabled="busy">Refresh</button>
      <button class="btn p" v-if="res.template || (!res.method && res.item)" @click="startCreate" style="margin-left:auto">Create</button>
    </div>
    <p class="err" v-if="err">{{ err }}</p>
    <template v-if="isList">
      <DataTable :cols="cols" :rows="rows" pick @pick="pick" />
      <div class="pager" v-if="cursor || stack.length"><button class="btn s" :disabled="!stack.length" @click="prev">Prev</button><button class="btn s" :disabled="!cursor" @click="next">Next</button></div>
    </template>
    <template v-else-if="data && typeof data === 'object'">
      <div class="grid" style="grid-template-columns:repeat(3,1fr)">
        <div v-for="(v, k) in data" :key="String(k)" style="border:0;margin:0"><Kv :k="String(k).replace(/_/g, ' ')" :v="typeof v === 'object' && v !== null ? undefined : v" :mono="typeof v === 'string'">
          <JsonView v-if="typeof v === 'object' && v !== null" :value="v" /><template v-else>{{ v ?? '—' }}</template></Kv></div>
      </div>
      <div class="tools"><button class="btn" @click="load()">Refresh</button></div>
    </template>
    <div v-else-if="!err && !busy" class="empty">No Data</div>
  </div>
  <Drawer v-if="open" :title="res.title.replace(/s$/, '')" @close="open = null">
    <JsonView v-if="!res.item" :value="open" />
    <template v-else>
      <textarea class="in" v-model="edit" style="min-height:360px"></textarea>
      <div class="form"><div class="row"><button class="btn d" @click="remove">Delete</button><button class="btn" @click="open = null">Close</button><button class="btn p" @click="save">Save</button></div></div>
    </template>
  </Drawer>
  <Drawer v-if="creating" :title="'New ' + res.title.replace(/s$/, '').toLowerCase()" @close="creating = false">
    <textarea class="in" v-model="text" style="min-height:300px"></textarea>
    <div class="form"><div class="row"><button class="btn" @click="creating = false">Cancel</button><button class="btn p" @click="create">Create</button></div></div>
  </Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
