<script setup lang="ts">
// Legal documents players must accept (ToS, privacy, marketing…): versions per key and locale,
// who they apply to, acceptance counts, and the consent trail per document.
import { ref, onMounted } from 'vue'
import { get, post, put, qs } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'doc' | 'consents'>(''); const form = ref<any>({}); const key = ref(''); const accepted = ref('')
async function load() { err.value = ''; try { x.value.l = await get('/v1/admin/legal' + qs({ key: key.value || undefined })) } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
function openDoc(r?: any) { dlg.value = 'doc'; form.value = r ? { key: r.key, version: r.version + 1, locale: r.locale, title: r.title, url: r.url, body: '', mandatory: r.mandatory, countries: (r.countries ?? []).join(', '), grace_secs: r.grace_secs ?? 0 } : { key: '', version: 1, locale: 'en', title: '', url: '', body: '', mandatory: true, countries: '', grace_secs: 0 } }
async function save() { const f = form.value; await act(() => put(`/v1/admin/legal/${encodeURIComponent(f.key)}/${f.version}`, { locale: f.locale, title: f.title, url: f.url || undefined, body: f.body || undefined, mandatory: f.mandatory, countries: f.countries.split(',').map((s: string) => s.trim().toUpperCase()).filter(Boolean), grace_secs: Number(f.grace_secs) || 0 }), 'published') }
async function consents(r: any, acc?: string) { dlg.value = 'consents'; x.value.doc = r; accepted.value = acc ?? ''; try { x.value.c = await get(`/v1/admin/legal/${encodeURIComponent(r.key)}/consents` + qs({ version: r.version, accepted: acc || undefined, limit: 200 })) } catch (e: any) { x.value.c = { error: e.message } } }
const docs = () => x.value.l?.documents ?? []
</script>
<template>
  <div class="crumb"><span>Legal documents</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="openDoc()">Publish document</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tools"><input class="in" v-model="key" placeholder="filter by key (tos, privacy, …)" @keyup.enter="load" /><button class="btn" @click="load">Refresh</button><span class="dim">{{ docs().filter((d: any) => d.published && !d.retired_at).length }} live · {{ docs().filter((d: any) => d.retired_at).length }} retired</span></div>
    <DataTable :cols="[{ key: 'key', mono: true }, { key: 'version', fmt: (r) => 'v' + r.version }, { key: 'locale' }, { key: 'title' }, { key: 'mandatory', fmt: (r) => r.mandatory ? 'mandatory' : 'optional' }, { key: 'countries', fmt: (r) => (r.countries ?? []).join(' ') || 'everywhere' }, { key: 'grace_secs', label: 'grace', fmt: (r) => r.grace_secs ? Math.round(r.grace_secs / 86400) + ' d' : 'none' }, { key: 'accepted_count', label: 'accepted / declined', fmt: (r) => `${r.accepted_count} / ${r.declined_count}` }, { key: 'state', fmt: (r) => r.retired_at ? 'retired ' + ts(r.retired_at) : r.published ? 'live since ' + ts(r.published_at) : 'draft' }]" :rows="docs()" empty="No legal documents — publish a ToS/privacy version to gate sign-in on acceptance">
      <template #actions="{ row }"><button class="btn s" @click="consents(row)">Consents</button><button class="btn s" @click="openDoc(row)">New version</button><button class="btn s d" v-if="!row.retired_at" @click="ask(`Retire ${row.key} v${row.version}? Players are no longer asked to accept it.`) && act(() => post(`/v1/admin/legal/${encodeURIComponent(row.key)}/${row.version}/retire`), 'retired')">Retire</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">A new mandatory version blocks players (403 with the document) until they accept; <code>grace</code> lets them keep playing that long first. Country codes (ISO, <code>ZZ</code> = unknown) scope a document to jurisdictions.</p>
  </div>
  <Drawer v-if="dlg === 'doc'" :title="form.key ? `${form.key} v${form.version}` : 'Publish document'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Key</label><input class="in" v-model="form.key" placeholder="tos" /></div>
    <div class="f"><label>Version / locale</label><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="form.version" /><input class="in" v-model="form.locale" placeholder="en" /></div></div>
    <div class="f"><label>Title</label><input class="in" v-model="form.title" /></div>
    <div class="f"><label>URL</label><input class="in" v-model="form.url" placeholder="https://example.com/tos" /></div>
    <div class="f"><label>Body (optional, served to clients)</label><textarea class="in" v-model="form.body" style="min-height:120px"></textarea></div>
    <div class="f"><label>Countries (comma-separated ISO; empty = everywhere)</label><input class="in" v-model="form.countries" placeholder="DE, FR, ZZ" /></div>
    <div class="f"><label>Grace (seconds)</label><input class="in" type="number" v-model.number="form.grace_secs" /></div>
    <div class="f"><label><input type="checkbox" v-model="form.mandatory" /> mandatory</label></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.key || !form.title" @click="save">Publish</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'consents'" :title="`Consents · ${x.doc?.key} v${x.doc?.version}`" @close="dlg = ''">
    <div class="tools"><select class="in" v-model="accepted" @change="consents(x.doc, accepted)"><option value="">all</option><option value="true">accepted</option><option value="false">declined</option></select></div>
    <DataTable v-if="x.c && !x.c.error" :cols="[{ key: 'account_id', mono: true }, { key: 'accepted', fmt: (r) => r.accepted ? 'accepted' : 'declined' }, { key: 'locale' }, { key: 'at', fmt: (r) => ts(r.at ?? r.created_at), dim: true }]" :rows="x.c.consents ?? []" empty="No consents recorded" />
    <JsonView v-else-if="x.c" :value="x.c" />
  </Drawer>
</template>
