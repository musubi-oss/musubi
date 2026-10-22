<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const name = computed(() => String(route.params.name)); const M = computed(() => `/v1/admin/runtime/modules/${encodeURIComponent(name.value)}`)
const mod = ref<any>(null); const all = ref<any>(null); const versions = ref<any[]>([]); const canary = ref<any>(null); const env = ref<{ k: string; v: string }[]>([]); const missing = ref<string[]>([]); const err = ref(''); const dlg = ref(''); const form = ref<any>({}); const file = ref<File | null>(null)
async function load() {
  err.value = ''
  try {
    all.value = await get('/v1/admin/runtime/modules'); mod.value = (all.value.stored ?? []).find((m: any) => m.name === name.value) ?? { name: name.value }
    const v = await get(`${M.value}/versions`).catch(() => ({})); versions.value = v.versions ?? []; const c = versions.value.find((x: any) => x.state === 'canary'); canary.value = c ? { version: c.version, percent: c.canary_percent, accounts: c.canary_accounts ?? [] } : null
    const e = await get(`${M.value}/env`).catch(() => ({})); const list = Array.isArray(e.env) ? e.env : Object.entries(e.env ?? {}).map(([key, value]) => ({ key, value })); env.value = list.map((x: any) => ({ k: x.key ?? x.name ?? x.k ?? '', v: String(x.value ?? x.v ?? '') })); missing.value = e.missing ?? []
  } catch (e: any) { err.value = e.message }
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
async function upload() {
  if (!file.value) return
  const buf = await file.value.arrayBuffer()
  await act(() => api('PUT', M.value, buf, { headers: { 'content-type': 'application/wasm' } }), 'module uploaded and loaded')
}
const ts = (v: any) => v ? new Date(typeof v === 'number' ? v * 1000 : v).toISOString().replace('T', ' ').slice(0, 19) : '—'
const kb = (n: any) => n ? (n / 1024).toFixed(1) + ' KB' : '—'
</script>
<template>
  <div class="crumb"><router-link to="/content/modules">WASM modules</router-link> › <span class="m">{{ name }}</span><span class="id" v-if="mod?.sha256">sha256 {{ mod.sha256.slice(0, 16) }}…</span>
    <span style="margin-left:auto;display:flex;gap:8px">
      <button class="btn s" @click="dlg = 'upload'; file = null">Upload version</button>
      <button class="btn s" @click="dlg = 'canary'; form = { version: versions.find((x) => x.state !== 'active')?.version ?? '', percent: 10, accounts: '' }">Canary</button>
      <button class="btn s" v-if="canary" @click="ask('Promote canary to 100%?') && act(() => post(`${M}/canary/promote`), 'promoted')">Promote</button>
      <button class="btn s" v-if="canary" @click="act(() => api('DELETE', `${M}/canary`), 'canary dropped')">Drop canary</button>
      <button class="btn s d" @click="ask('Unload and delete this module and all its versions?') && act(async () => { await api('DELETE', M); router.push('/content/modules') }, 'deleted')">Delete</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="mod">
    <div class="grid">
      <div><Kv k="Loaded"><span :class="mod.loaded ? 'ok' : 'bad'">{{ mod.loaded ? 'yes — serving hooks / RPCs' : 'no' }}</span></Kv><Kv k="Size" :v="kb(mod.size)" /></div>
      <div><Kv k="Updated" :v="ts(mod.updated_at)" /><Kv k="sha256" :v="mod.sha256 ?? '—'" mono /></div>
      <div><Kv k="Canary"><template v-if="canary">version {{ canary.version }} at {{ canary.percent }}% of accounts{{ canary.accounts.length ? ` + ${canary.accounts.length} pinned` : '' }}</template><span class="dim" v-else>none — all traffic on the active version</span></Kv><Kv k="RPCs exported by runtime" :v="(all?.rpcs ?? []).join(', ') || '—'" /></div>
    </div>
    <h3>Versions <small>({{ versions.length }})</small></h3>
    <DataTable :cols="[{ key: 'version' }, { key: 'sha256', fmt: (r) => String(r.sha256 ?? '').slice(0, 16) + '…', dim: true }, { key: 'size', fmt: (r) => kb(r.size) }, { key: 'created_at', fmt: (r) => ts(r.created_at ?? r.updated_at), dim: true }, { key: 'state', fmt: (r) => r.state === 'canary' ? `canary ${r.canary_percent}%` : r.state }, { key: 'note', fmt: (r) => r.note ?? '', dim: true }]" :rows="versions">
      <template #actions="{ row }"><button class="btn s" v-if="row.state !== 'active'" @click="ask(`Roll back to version ${row.version}?`) && act(() => post(`${M}/versions/${row.version}/rollback`), 'rolled back')">Rollback</button></template>
    </DataTable>
    <h3>Environment <small>({{ env.length }} — visible to the module as env vars{{ missing.length ? `; missing required: ${missing.join(', ')}` : '' }})</small> <button class="btn s p" style="margin-left:8px" @click="env.push({ k: '', v: '' })">Add</button> <button class="btn s" @click="act(() => put(`${M}/env`, { values: Object.fromEntries(env.filter((e) => e.k).map((e) => [e.k, e.v])), secrets: [] }), 'env saved')">Save env</button></h3>
    <table><thead><tr><th style="width:280px">Key</th><th>Value</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(e, i) in env" :key="i"><td><input class="in m" style="min-width:0;width:100%" v-model="e.k" /></td><td><input class="in m" style="min-width:0;width:100%" v-model="e.v" /></td><td><button class="btn s d" @click="e.k ? act(() => api('DELETE', `${M}/env/${encodeURIComponent(e.k)}`), 'removed') : env.splice(i, 1)">×</button></td></tr>
      <tr v-if="!env.length"><td colspan="3" class="empty">No variables</td></tr></tbody></table>
  </div>
  <Drawer v-if="dlg === 'upload'" title="Upload module version" @close="dlg = ''"><div class="form"><p class="note">A new .wasm replaces the active version immediately (the previous one stays in the version list for rollback). Use a canary to test it on a share of calls first.</p><div class="f"><label>.wasm file</label><input type="file" accept=".wasm,application/wasm" @change="(e: any) => (file = e.target.files?.[0] ?? null)" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!file" @click="upload">Upload</button></div></div></Drawer>
  <Drawer v-if="dlg === 'canary'" title="Canary release" @close="dlg = ''"><div class="form"><div class="f"><label>Version</label><select class="sel" v-model="form.version"><option v-for="v in versions.filter((x) => x.state !== 'active')" :key="v.version" :value="v.version">v{{ v.version }} · {{ v.state }} · {{ String(v.sha256 ?? '').slice(0, 12) }}</option></select></div><div class="f"><label>Percent of accounts</label><input class="in" type="number" min="0" max="99" v-model.number="form.percent" /></div><div class="f"><label>Pinned account ids (comma-separated, always on canary)</label><input class="in m" v-model="form.accounts" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.version" @click="act(() => post(`${M}/canary`, { version: Number(form.version), percent: Number(form.percent), accounts: form.accounts.split(',').map((x: string) => x.trim()).filter(Boolean) }), 'canary set')">Start</button></div></div></Drawer>
</template>
