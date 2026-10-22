<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new'); const I = computed(() => `/v1/server/items/${encodeURIComponent(id.value)}`)
const it = ref<any>(null); const err = ref(''); const versions = ref<any[]>([]); const dlg = ref(''); const form = ref<any>({}); const props = ref('{}'); const schema = ref(''); const slots = ref('')
async function load() {
  err.value = ''
  try {
    if (isNew.value) it.value = { id: '', name: '', category: '', stackable: true, max_stack: 999, tradeable: true, unique: false, bind: 'none', properties: {}, slots: [] }
    else { const r = await get(I.value); it.value = r.item ?? r; versions.value = ((await get(`${I.value}/versions`).catch(() => ({}))).versions ?? []) }
    props.value = JSON.stringify(it.value.properties ?? {}, null, 2); schema.value = it.value.instance_schema ? JSON.stringify(it.value.instance_schema, null, 2) : ''; slots.value = (it.value.slots ?? []).join(', ')
  } catch (e: any) { err.value = e.message }
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; load(); return r } catch (e: any) { toast.err(e) } }
function body() { return { ...it.value, id: it.value.id || undefined, properties: JSON.parse(props.value || '{}'), instance_schema: schema.value.trim() ? JSON.parse(schema.value) : null, slots: slots.value.split(',').map((s) => s.trim()).filter(Boolean), max_stack: Number(it.value.max_stack) } }
async function save() { const b = body(); const r = await act(() => post('/v1/server/items', b), isNew.value ? 'item created' : 'new item version saved'); if (r && isNew.value) router.replace(`/economy/items/${encodeURIComponent(r.id ?? r.item?.id ?? b.id)}`) }
function open(k: string, f: any) { dlg.value = k; form.value = f }
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
</script>
<template>
  <div class="crumb"><router-link to="/economy/items">Items</router-link> › <span>{{ isNew ? 'new item' : it?.name || id }}</span><span class="id" v-if="!isNew">{{ id }} · v{{ it?.version }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="it">
      <template v-if="!isNew">
        <button class="btn s" @click="open('status', { status: it.status ?? 'active' })">Status</button>
        <button class="btn s" @click="open('lifecycle', { deprecated_at: '', sunset_at: '', convert_to: '' })">Lifecycle</button>
        <button class="btn s" @click="open('migrate', { to_version: it.version })">Migrate instances</button>
        <button class="btn s" @click="open('convert', { to: '', ratio: 1 })">Convert</button>
        <button class="btn s d" @click="open('revoke', { reason: '' })">Revoke all</button>
      </template>
      <button class="btn s p" @click="save">{{ isNew ? 'Create' : 'Save as new version' }}</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="it">
    <div class="grid">
      <div><Kv k="Id"><input class="in m" v-model="it.id" :disabled="!isNew" placeholder="sword_iron" /></Kv><Kv k="Name"><input class="in" v-model="it.name" /></Kv><Kv k="Category"><input class="in" v-model="it.category" placeholder="weapon" /></Kv></div>
      <div><Kv k="Stacking"><label><input type="checkbox" v-model="it.stackable" /> stackable</label> <input class="in" type="number" min="1" v-model.number="it.max_stack" style="max-width:120px;margin-left:10px" :disabled="!it.stackable" /></Kv><Kv k="Trading"><label><input type="checkbox" v-model="it.tradeable" /> tradeable</label></Kv><Kv k="Unique"><label><input type="checkbox" v-model="it.unique" /> one per account</label></Kv></div>
      <div><Kv k="Bind"><select class="sel" v-model="it.bind"><option value="none">none</option><option value="on_pickup">on pickup</option><option value="on_equip">on equip</option><option value="on_use">on use</option></select></Kv><Kv k="Equipment slots (comma-separated)"><input class="in" v-model="slots" placeholder="main_hand, off_hand" /></Kv><Kv k="Status" :v="it.status ?? 'active'" /></div>
    </div>
    <div class="grid" style="margin-top:8px">
      <div><Kv k="Properties (JSON, shared by every instance)"><textarea class="in m" v-model="props" style="min-height:160px"></textarea></Kv></div>
      <div><Kv k="Instance schema (JSON Schema for per-instance data, optional)"><textarea class="in m" v-model="schema" style="min-height:160px" placeholder="{ }"></textarea></Kv></div>
      <div><Kv k="Migrations" :v="(it.migrations ?? []).length ? JSON.stringify(it.migrations) : 'none'" /><Kv k="Lifecycle" :v="it.deprecated_at || it.sunset_at ? `deprecated ${ts(it.deprecated_at)} · sunset ${ts(it.sunset_at)}${it.convert_to ? ' → ' + it.convert_to : ''}` : 'active, no sunset'" /></div>
    </div>
    <template v-if="!isNew">
      <h3>Versions <small>({{ versions.length }})</small></h3>
      <DataTable :cols="[{ key: 'version' }, { key: 'name' }, { key: 'properties', fmt: (r) => JSON.stringify(r.properties ?? {}).slice(0, 100), dim: true }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="versions">
        <template #actions="{ row }"><button class="btn s" v-if="row.version !== it.version" @click="ask(`Roll back to version ${row.version}?`) && act(() => post(`${I}/versions/${row.version}/rollback`), 'rolled back')">Rollback</button></template>
      </DataTable>
      <h3>Raw</h3><JsonView :value="it" />
    </template>
  </div>
  <Drawer v-if="dlg === 'status'" title="Item status" @close="dlg = ''"><div class="form"><div class="f"><label>Status</label><select class="sel" v-model="form.status"><option value="active">active — grantable and usable</option><option value="disabled">disabled — hidden, instances frozen</option><option value="deprecated">deprecated — no new grants</option></select></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${I}/status`, form), 'status set')">Apply</button></div></div></Drawer>
  <Drawer v-if="dlg === 'lifecycle'" title="Lifecycle" @close="dlg = ''"><div class="form"><p class="note">Deprecation stops new grants at that time; sunset removes instances, converting them to another item if set.</p><div class="f"><label>Deprecated at</label><input class="in" type="datetime-local" v-model="form.deprecated_at" /></div><div class="f"><label>Sunset at</label><input class="in" type="datetime-local" v-model="form.sunset_at" /></div><div class="f"><label>Convert to item id</label><input class="in m" v-model="form.convert_to" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => api('PUT', `${I}/lifecycle`, { deprecated_at: form.deprecated_at ? Math.floor(new Date(form.deprecated_at).getTime() / 1000) : null, sunset_at: form.sunset_at ? Math.floor(new Date(form.sunset_at).getTime() / 1000) : null, convert_to: form.convert_to || null }), 'lifecycle set')">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'migrate'" title="Migrate instances" @close="dlg = ''"><div class="form"><p class="note">Rewrites every player's instance data to the chosen version using the item's migrations.</p><div class="f"><label>To version</label><input class="in" type="number" v-model.number="form.to_version" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${I}/migrate`, form), 'migration started')">Migrate</button></div></div></Drawer>
  <Drawer v-if="dlg === 'convert'" title="Convert instances" @close="dlg = ''"><div class="form"><div class="f"><label>Target item id</label><input class="in m" v-model="form.to" /></div><div class="f"><label>Ratio (source → target)</label><input class="in" type="number" min="0" step="0.01" v-model.number="form.ratio" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.to" @click="ask('Convert every instance?') && act(() => post(`${I}/convert`, form), 'converted')">Convert</button></div></div></Drawer>
  <Drawer v-if="dlg === 'revoke'" title="Revoke every instance" @close="dlg = ''"><div class="form"><p class="note">Removes this item from every inventory and writes a ledger entry per account.</p><div class="f"><label>Reason</label><input class="in" v-model="form.reason" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn d" :disabled="!form.reason" @click="ask('Revoke from ALL players?') && act(() => post(`${I}/revoke`, form), 'revoked')">Revoke</button></div></div></Drawer>
</template>
