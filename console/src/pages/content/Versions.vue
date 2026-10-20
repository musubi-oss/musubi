<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const router = useRouter()
const rows = ref<any[]>([]); const err = ref(''); const dlg = ref(false); const text = ref(''); const note = ref(''); const dry = ref(true); const result = ref<any>(null); const loading = ref(false)
async function load() { err.value = ''; try { rows.value = (await get('/v1/admin/content/versions')).versions ?? [] } catch (e: any) { err.value = e.message } }
onMounted(load)
const latest = computed(() => rows.value[0])
const total = (r: any) => Object.values(r.counts ?? {}).reduce((a: number, b: any) => a + Number(b || 0), 0)
function delta(i: number) { const a = rows.value[i], b = rows.value[i + 1]; if (!a || !b) return ''; const out: string[] = []; for (const k of Object.keys(a.counts ?? {})) { const d = Number(a.counts[k] ?? 0) - Number(b.counts?.[k] ?? 0); if (d) out.push(`${k} ${d > 0 ? '+' : ''}${d}`) } return out.join(', ') }
async function exportBundle() { try { const r: Response = await api('GET', '/v1/admin/content', undefined, { raw: true }); const blob = await r.blob(); const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = `content-${new Date().toISOString().slice(0, 10)}.json`; a.click() } catch (e: any) { toast.err(e) } }
async function pick(ev: Event) { const f = (ev.target as HTMLInputElement).files?.[0]; if (f) text.value = await f.text() }
async function doImport() {
  loading.value = true
  try { result.value = await post(`/v1/admin/content/import?dry_run=${dry.value}${note.value ? '&note=' + encodeURIComponent(note.value) : ''}`, JSON.parse(text.value)); toast.show(dry.value ? 'dry run complete' : 'imported'); if (!dry.value) { dlg.value = false; load() } } catch (e: any) { toast.err(e) } finally { loading.value = false }
}
const ts = (v: any) => v ? String(v).replace('T', ' ').slice(0, 19) : '—'
</script>
<template>
  <div class="crumb"><span>Content versions</span><span class="id" v-if="latest">latest #{{ latest.id }} · {{ total(latest) }} rows</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="exportBundle">Export current</button><button class="btn s p" @click="dlg = true; result = null">Import bundle</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Every import, rollback and in-console edit of economy / content tables snapshots the whole bundle as a version. Open one to see per-table counts and the diff against its predecessor.</p>
    <DataTable :cols="[{ key: 'id', fmt: (r) => '#' + r.id }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'source' }, { key: 'note', fmt: (r) => r.note ?? '' }, { key: 'sections', fmt: (r) => (r.sections ?? []).join(', '), dim: true }, { key: 'rows', fmt: (r) => total(r) }, { key: 'change', fmt: (r) => delta(rows.indexOf(r)) || '—', dim: true }]" :rows="rows" pick @pick="(r) => router.push(`/content/bundles/${r.id}`)" />
  </div>
  <Drawer v-if="dlg" title="Import content bundle" @close="dlg = false"><div class="form">
    <p class="note">Paste or pick a bundle JSON (the format exported above). Dry run validates and reports what would change without writing.</p>
    <div class="f"><label>File</label><input type="file" accept="application/json,.json" @change="pick" /></div>
    <div class="f"><label>Bundle JSON</label><textarea class="in m" v-model="text" style="min-height:200px"></textarea></div>
    <div class="f"><label>Note</label><input class="in" v-model="note" placeholder="season 4 balance pass" /></div>
    <div class="f"><label><input type="checkbox" v-model="dry" /> dry run</label></div>
    <pre class="pre" v-if="result" style="max-height:220px">{{ JSON.stringify(result, null, 2) }}</pre>
    <div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" :disabled="!text.trim() || loading" @click="doImport">{{ dry ? 'Validate' : 'Import' }}</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
