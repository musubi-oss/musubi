<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const schemas = ref<any[]>([]); const err = ref(''); const q = ref(''); const dlg = ref(''); const s = ref<any>(null); const isNew = ref(false); const schemaTxt = ref(''); const idx = ref<{ field: string; kind: string; unique: boolean }[]>([]); const mig = ref({ from_version: 1, script: '' })
async function load() { err.value = ''; try { schemas.value = (await get('/v1/admin/storage_schemas')).schemas ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
const view = computed(() => schemas.value.filter((x) => !q.value || x.collection.toLowerCase().includes(q.value.toLowerCase())))
function edit(x: any) { isNew.value = !x; s.value = JSON.parse(JSON.stringify(x ?? { collection: '', version: 1, schema: { type: 'object', properties: {}, required: [] }, indexes: [], migrations: [] })); schemaTxt.value = JSON.stringify(s.value.schema ?? {}, null, 2); idx.value = (s.value.indexes ?? []).map((i: any) => ({ field: i.field, kind: i.kind ?? 'string', unique: !!i.unique })); dlg.value = 'edit' }
async function save() { try { await put(`/v1/admin/storage_schemas/${encodeURIComponent(s.value.collection)}`, { version: Number(s.value.version), schema: JSON.parse(schemaTxt.value || '{}'), indexes: idx.value.filter((i) => i.field) }); toast.show('schema saved'); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(r?.migrated != null ? `${ok}: ${r.migrated} objects` : ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
const props = (x: any) => Object.keys(x.schema?.properties ?? {})
const pg1 = usePager(() => view)
</script>
<template>
  <div class="crumb"><span>Storage schemas</span><span class="id">{{ schemas.length }} collections validated</span><span style="margin-left:auto;display:flex;gap:8px"><input class="in" v-model="q" placeholder="filter collection" style="width:200px" /><button class="btn s p" @click="edit(null)">New schema</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">A JSON Schema per storage collection: writes that do not validate are rejected with the schema path; indexes make <code>query</code> on that field fast; bumping the version with a migration rewrites existing objects.</p>
    <table><thead><tr><th>Collection</th><th style="width:70px">Version</th><th>Properties</th><th>Indexes</th><th style="width:90px">Migrations</th><th style="width:170px"></th></tr></thead><tbody>
      <tr v-for="x in pg1.rows" :key="x.collection"><td class="m row" @click="edit(x)">{{ x.collection }}</td><td>{{ x.version }}</td><td class="dim">{{ props(x).map((p) => (x.schema.required ?? []).includes(p) ? p + '*' : p).join(', ') || '—' }}</td><td class="dim">{{ (x.indexes ?? []).map((i: any) => `${i.field}${i.unique ? ' (unique)' : ''}`).join(', ') || '—' }}</td><td class="dim">{{ (x.migrations ?? []).length }}</td>
        <td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" @click="s = x; mig = { from_version: x.version, script: '' }; dlg = 'migrate'">Migrate</button> <button class="btn s" @click="ask(`Drop schema for ${x.collection}? Objects stay, validation stops.`) && act(() => api('DELETE', `/v1/admin/storage_schemas/${encodeURIComponent(x.collection)}`), 'schema removed')">×</button></td></tr>
      <tr v-if="!view.length"><td colspan="6" class="empty">No schemas — collections accept any JSON</td></tr></tbody></table><Pager :p="pg1" />
  </div>
  <Drawer v-if="dlg === 'edit'" :title="isNew ? 'New storage schema' : `Schema ${s.collection}`" @close="dlg = ''"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Collection</label><input class="in m" v-model="s.collection" :disabled="!isNew" /></div><div><label>Version</label><input class="in" type="number" min="1" v-model.number="s.version" style="width:90px" /></div></div>
    <div class="f"><label>JSON Schema</label><textarea class="in m" v-model="schemaTxt" style="min-height:220px"></textarea></div>
    <h3>Indexes <button class="btn s p" style="margin-left:8px" @click="idx.push({ field: '', kind: 'string', unique: false })">Add</button></h3>
    <div class="f" v-for="(i, n) in idx" :key="n" style="display:flex;gap:8px"><input class="in m" v-model="i.field" placeholder="field" style="flex:1" /><select class="sel" v-model="i.kind"><option>string</option><option>number</option><option>bool</option></select><label><input type="checkbox" v-model="i.unique" /> unique</label><button class="btn s" @click="idx.splice(n, 1)">×</button></div>
    <template v-if="s.migrations?.length"><h3>Migration history</h3><JsonView :value="s.migrations" /></template>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!s.collection" @click="save">Save</button></div></div></Drawer>
  <Drawer v-if="dlg === 'migrate'" :title="`Migrate ${s.collection}`" @close="dlg = ''"><div class="form"><p class="note">Rewrites every object stored at <i>from version</i> with the script (a JSON patch array or a runtime module function name), stamping them with the current version.</p>
    <div class="f"><label>From version</label><input class="in" type="number" min="1" v-model.number="mig.from_version" style="width:90px" /></div><div class="f"><label>Patch / function</label><textarea class="in m" v-model="mig.script" style="min-height:120px" placeholder='[{"op":"add","path":"/level","value":1}]'></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`/v1/admin/storage_schemas/${encodeURIComponent(s.collection)}/migrate`, { from_version: Number(mig.from_version), patch: (() => { try { return JSON.parse(mig.script) } catch { return mig.script } })() }), 'migrated')">Run migration</button></div></div></Drawer>
</template>
