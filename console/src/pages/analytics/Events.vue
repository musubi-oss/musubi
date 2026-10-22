<script setup lang="ts">
// Raw event explorer and the schemas that validate events on ingest.
import { ref, onMounted } from 'vue'
import { api, get, post, put, qs } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const f = ref<any>({ name: '', account_id: '', platform: '', limit: 200 }); const dlg = ref(''); const form = ref<any>({})
async function load() { err.value = ''; try { const [e, s] = await Promise.all([get('/v1/server/analytics/events' + qs({ name: f.value.name || undefined, account_id: f.value.account_id || undefined, platform: f.value.platform || undefined, limit: f.value.limit })), get('/v1/server/analytics/schemas').catch(() => ({ schemas: [] }))]); x.value.e = e; x.value.s = s } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 19) : '—'
function openSchema(s?: any) { dlg.value = 'schema'; form.value = { name: s?.name ?? '', text: JSON.stringify(s ? { ...s, name: undefined } : { properties: { level: { type: 'integer' } }, required: ['level'], strict: false }, null, 2), existing: !!s } }
async function saveSchema() { let body: any; try { body = JSON.parse(form.value.text) } catch { toast.err(new Error('schema must be JSON')); return } await act(() => form.value.existing ? put(`/v1/server/analytics/schemas/${encodeURIComponent(form.value.name)}`, body) : post('/v1/server/analytics/schemas', { name: form.value.name, ...body }), 'schema saved') }
</script>
<template>
  <div class="crumb"><span>Events</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tools"><input class="in" v-model="f.name" placeholder="event name" @keyup.enter="load" /><input class="in" v-model="f.account_id" placeholder="account id" @keyup.enter="load" /><input class="in" v-model="f.platform" placeholder="platform" style="max-width:120px" @keyup.enter="load" /><select class="in" v-model.number="f.limit"><option :value="50">50</option><option :value="200">200</option><option :value="1000">1 000</option></select><button class="btn p" @click="load">Search</button><span class="dim">{{ (x.e?.events ?? []).length }} events, newest first</span></div>
    <DataTable :cols="[{ key: 'at', fmt: (r) => ts(r.at), dim: true }, { key: 'name', mono: true }, { key: 'account_id', mono: true }, { key: 'platform' }, { key: 'country' }, { key: 'source' }, { key: 'properties', fmt: (r) => JSON.stringify(r.properties ?? {}), dim: true }]" :rows="x.e?.events ?? []" pick @pick="(r) => (x.pick = r)" empty="No events match" />
    <p class="note" style="margin-top:10px">Clients and servers post events to <code>/v1/analytics/events</code>; sampling (overview) decides what is kept, schemas below decide what is valid.</p>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Schemas <small>({{ (x.s?.schemas ?? []).length }})</small><button class="btn s p" style="margin-left:10px" @click="openSchema()">Add schema</button></h3>
    <DataTable :cols="[{ key: 'name', mono: true }, { key: 'required', fmt: (r) => (r.required ?? []).join(', ') }, { key: 'properties', fmt: (r) => Object.keys(r.properties ?? {}).join(', '), dim: true }, { key: 'strict', fmt: (r) => r.strict ? 'strict — unknown properties rejected' : 'lenient' }]" :rows="x.s?.schemas ?? []" empty="No schemas — every event is accepted as sent">
      <template #actions="{ row }"><button class="btn s" @click="openSchema(row)">Edit</button><button class="btn s d" @click="ask(`Delete schema ${row.name}?`) && act(() => api('DELETE', `/v1/server/analytics/schemas/${encodeURIComponent(row.name)}`), 'deleted')">Delete</button></template>
    </DataTable>
  </div>
  <Drawer v-if="x.pick" title="Event" @close="x.pick = null"><JsonView :value="x.pick" /></Drawer>
  <Drawer v-if="dlg === 'schema'" :title="form.existing ? form.name : 'New schema'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Event name</label><input class="in" v-model="form.name" :disabled="form.existing" placeholder="level_up" /></div>
    <div class="f"><label>Schema (JSON: properties, required, strict)</label><textarea class="in" v-model="form.text" style="min-height:200px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.name" @click="saveSchema">Save</button></div>
  </div></Drawer>
</template>
