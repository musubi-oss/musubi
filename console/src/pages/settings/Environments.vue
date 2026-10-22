<script setup lang="ts">
// Environments (dev → staging → prod) and promotions between them: each row is another
// cluster reachable by URL; promote exports this cluster's content sections into it.
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'env' | 'promote'>(''); const form = ref<any>({}); const status = ref<Record<string, any>>({})
async function load() { err.value = ''; try { const [e, p] = await Promise.all([get('/v1/admin/environments'), get('/v1/admin/promotions?limit=50').catch(() => ({ promotions: [] }))]); x.value.env = e; x.value.promos = p } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; await load(); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
async function check(name: string) { try { status.value[name] = await get(`/v1/admin/environments/${encodeURIComponent(name)}/status`) } catch (e: any) { status.value[name] = { error: e.message } } }
function openEnv(r?: any) { dlg.value = 'env'; form.value = r ? { name: r.name, url: r.url, protected: !!r.protected, description: r.description ?? '' } : { name: '', url: '', protected: false, description: '' } }
function openPromote(r: any) { dlg.value = 'promote'; form.value = { to: r.name, dry_run: true, prune: false, note: '' }; x.value.promoteResult = null }
async function promote() { const f = form.value; try { x.value.promoteResult = await post(`/v1/admin/environments/${encodeURIComponent(f.to)}/promote`, { dry_run: f.dry_run, prune: f.prune, note: f.note }); toast.show(f.dry_run ? 'dry run finished' : 'promoted'); load() } catch (e: any) { toast.err(e) } }
</script>
<template>
  <div class="crumb"><span>Environments</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="openEnv()">Add environment</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <h3 style="margin-top:0">This cluster</h3>
    <div class="grid"><div><Kv k="Environment" :v="x.env?.this?.name" /></div><div><Kv k="Project" :v="x.env?.this?.project" /></div><div><Kv k="Known environments" :v="(x.env?.environments ?? []).length" /></div></div>
    <p class="note">The name comes from <code>[node] environment</code>; a promotion is refused when the target cluster says it is a different environment than the row names.</p>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Environments <small>({{ (x.env?.environments ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'name' }, { key: 'url', mono: true }, { key: 'protected', fmt: (r) => r.protected ? 'protected — promote asks twice' : '' }, { key: 'description', dim: true }, { key: 'status', fmt: (r) => status[r.name] ? (status[r.name].error ? 'unreachable: ' + status[r.name].error : `reachable · ${status[r.name].environment ?? status[r.name].name ?? 'ok'}${status[r.name].version ? ' · v' + status[r.name].version : ''}`) : '—' }]" :rows="x.env?.environments ?? []" empty="No other environments registered — add staging/prod by URL to promote content there">
      <template #actions="{ row }"><button class="btn s" @click="check(row.name)">Check</button><button class="btn s" @click="openEnv(row)">Edit</button><button class="btn s p" @click="openPromote(row)">Promote here</button><button class="btn s d" @click="ask(`Forget environment ${row.name}?`) && act(() => api('DELETE', `/v1/admin/environments/${encodeURIComponent(row.name)}`), 'removed')">Remove</button></template>
    </DataTable>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Promotions <small>({{ (x.promos?.promotions ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'from' }, { key: 'to' }, { key: 'sections', fmt: (r) => (r.sections ?? []).join(', ') }, { key: 'dry_run', fmt: (r) => r.dry_run ? 'dry run' : r.prune ? 'applied + prune' : 'applied' }, { key: 'ok', fmt: (r) => r.ok ? 'ok' : `failed (${r.result?.status ?? '?'})` }, { key: 'note', dim: true }]" :rows="x.promos?.promotions ?? []" pick @pick="(r) => (x.pick = r)" empty="No promotions yet" />
  </div>
  <Drawer v-if="x.pick" title="Promotion" @close="x.pick = null"><JsonView :value="x.pick" /></Drawer>
  <Drawer v-if="dlg === 'env'" :title="form.name ? 'Environment' : 'Add environment'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Name</label><input class="in" v-model="form.name" placeholder="staging" /></div>
    <div class="f"><label>API URL</label><input class="in" v-model="form.url" placeholder="https://api-staging.example.com" /></div>
    <div class="f"><label>Description</label><input class="in" v-model="form.description" /></div>
    <div class="f"><label><input type="checkbox" v-model="form.protected" /> protected (production) — promotions need a second confirmation</label></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.name || !form.url" @click="act(() => put(`/v1/admin/environments/${encodeURIComponent(form.name)}`, { url: form.url, protected: form.protected, description: form.description }), 'saved')">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'promote'" :title="`Promote to ${form.to}`" @close="dlg = ''"><div class="form">
    <p class="note">Exports this cluster's content (remote config, flags, items, store, live events, …) and imports it into <b>{{ form.to }}</b> as a new content version there; the target can roll back. Start with a dry run.</p>
    <div class="f"><label><input type="checkbox" v-model="form.dry_run" /> dry run — show what would change</label></div>
    <div class="f"><label><input type="checkbox" v-model="form.prune" /> prune — remove entries in the target that this cluster does not have</label></div>
    <div class="f"><label>Note</label><input class="in" v-model="form.note" placeholder="release 1.4" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Close</button><button class="btn" :class="form.dry_run ? 'p' : 'd'" @click="(form.dry_run || ask(`Promote to ${form.to} for real?`)) && promote()">{{ form.dry_run ? 'Dry run' : 'Promote' }}</button></div>
    <template v-if="x.promoteResult"><h3>Result <small :class="x.promoteResult.ok === false ? 'bad' : 'ok'">{{ x.promoteResult.ok === false ? 'failed' : 'ok' }}</small></h3><JsonView :value="x.promoteResult" /></template>
  </div></Drawer>
</template>
