<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const name = computed(() => String(route.params.name)); const C = computed(() => `/v1/server/configs/${encodeURIComponent(name.value)}`)
const c = ref<any>(null); const err = ref(''); const text = ref(''); const x = ref<Record<string, any>>({}); const diffFrom = ref<number | null>(null)
const vdlg = ref(false); const vform = ref({ variant: '', priority: 1, enabled: true, selector: '{\n  "countries": []\n}', patch: '{}' })
async function fx(k: string, p: string) { try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = e.status === 404 ? null : { error: e.message } } }
async function load() {
  err.value = ''
  try { const r = await get(C.value); c.value = r.config ?? r; text.value = JSON.stringify(c.value.value ?? {}, null, 2) } catch (e: any) { err.value = e.message }
  await Promise.all([fx('history', `${C.value}/history`), fx('variants', `${C.value}/variants`), fx('source', `${C.value}/source`)])
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); vdlg.value = false; load() } catch (e: any) { toast.err(e) } }
const list = (k: string, sub?: string) => { const v = x.value[k]; if (!v || v.error) return []; const a = sub ? v[sub] : Object.values(v).find(Array.isArray); return Array.isArray(a) ? a : [] }
async function diff() { try { x.value.diff = await get(`${C.value}/diff` + qs({ from: diffFrom.value ?? undefined })) } catch (e: any) { toast.err(e) } }
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
</script>
<template>
  <div class="crumb"><router-link to="/liveops/configs">Remote config</router-link> › <span class="m">{{ name }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="vdlg = true">Add variant</button><button class="btn s" v-if="x.source" @click="act(() => post(`${C}/source/sync`), 'synced')">Sync source</button><button class="btn s d" @click="ask(`Delete config ${name}?`) && act(async () => { await api('DELETE', C); router.push('/liveops/configs') }, 'deleted')">Delete</button><button class="btn s p" @click="act(() => api('PUT', C, { value: JSON.parse(text) }), 'saved as new version')">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="c">
    <div class="grid"><div><Kv k="Version" :v="c.version" /><Kv k="Checksum" :v="c.checksum" mono copy /></div><div><Kv k="Updated" :v="ts(c.updated_at)" /><Kv k="Variants" :v="list('variants').length" /></div><div><Kv k="Source" :v="x.source ? `${x.source.format ?? ''} ${x.source.url ?? ''}` : 'none (edited here)'" /><Kv k="Clients" v="GET /v1/config/{name} with country / version selectors; variants layer onto this base" /></div></div>
    <h3>Value <small>(JSON — saving creates version {{ (c.version ?? 0) + 1 }})</small></h3>
    <textarea class="in" v-model="text" style="min-height:260px"></textarea>
    <h3>Variants <small>({{ list('variants').length }})</small></h3>
    <DataTable :cols="[{ key: 'variant', mono: true }, { key: 'priority' }, { key: 'enabled' }, { key: 'selector', fmt: (r) => JSON.stringify(r.selector ?? {}), dim: true }, { key: 'patch', fmt: (r) => JSON.stringify(r.patch ?? {}).slice(0, 120), dim: true }, { key: 'updated_at', fmt: (r) => ts(r.updated_at), dim: true }]" :rows="list('variants')">
      <template #actions="{ row }"><button class="btn s d" @click="ask('Delete variant?') && act(() => api('DELETE', `${C}/variants/${encodeURIComponent(row.variant)}`), 'variant deleted')">Delete</button></template>
    </DataTable>
    <h3>History <small>({{ list('history').length }})</small></h3>
    <DataTable :cols="[{ key: 'version' }, { key: 'id', dim: true }, { key: 'checksum', mono: true, dim: true }, { key: 'note', dim: true }, { key: 'deleted' }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="list('history')">
      <template #actions="{ row }"><span style="display:flex;gap:6px;justify-content:flex-end"><button class="btn s" @click="diffFrom = row.id; diff()">Diff</button><button class="btn s d" v-if="row.version !== c.version" @click="ask(`Roll back to version ${row.version}?`) && act(() => post(`${C}/rollback`, { version: row.version }), 'rolled back')">Rollback</button></span></template>
    </DataTable>
    <template v-if="x.diff"><h3>Diff <small>(history #{{ diffFrom }} → current)</small></h3><JsonView :value="x.diff" /></template>
  </div>
  <Drawer v-if="vdlg" title="Add variant" @close="vdlg = false"><div class="form">
    <div class="f"><label>Variant name</label><input class="in" v-model="vform.variant" /></div>
    <div class="f"><label>Selector (JSON: countries, versions, platforms, audience…)</label><textarea class="in" v-model="vform.selector" style="min-height:100px"></textarea></div>
    <div class="f"><label>Priority (higher wins) / enabled</label><div style="display:flex;gap:8px;align-items:center"><input class="in" type="number" v-model.number="vform.priority" style="max-width:120px" /><label><input type="checkbox" v-model="vform.enabled" /> enabled</label></div></div>
    <div class="f"><label>Patch (JSON, merged over base value)</label><textarea class="in" v-model="vform.patch" style="min-height:140px"></textarea></div>
    <div class="row"><button class="btn" @click="vdlg = false">Cancel</button><button class="btn p" :disabled="!vform.variant" @click="act(() => api('PUT', `${C}/variants/${encodeURIComponent(vform.variant)}`, { selector: JSON.parse(vform.selector), patch: JSON.parse(vform.patch), priority: vform.priority, enabled: vform.enabled }), 'variant saved')">Save</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
