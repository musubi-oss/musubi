<script setup lang="ts">
// Account deletions kept as a tombstone list so a restore, an import or a late write cannot
// resurrect a deleted player; sweeps re-apply them. Lifting a tombstone is an audited act.
import { ref, onMounted } from 'vue'
import { get, post } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'import' | 'lift'>(''); const form = ref<any>({}); const q = ref('')
async function load() { err.value = ''; try { x.value.d = await get('/v1/admin/deletions?limit=500') } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; load(); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
const rows = () => (x.value.d?.deletions ?? []).filter((r: any) => !q.value || r.account_id.includes(q.value) || (r.reason ?? '').includes(q.value))
async function doImport() { let entries: any[]; try { entries = form.value.text.split('\n').map((l: string) => l.trim()).filter(Boolean).map((l: string) => { const [account_id, reason] = l.split(/[,;\t]/).map((s: string) => s.trim()); return { account_id, reason: reason || undefined } }) } catch { toast.err(new Error('one account id per line')); return } x.value.imp = await act(() => post('/v1/admin/deletions/import', { entries }), `${entries.length} tombstones imported`) }
</script>
<template>
  <div class="crumb"><span>Deletions</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="ask('Re-apply every tombstone to data that may have come back?') && act(() => post('/v1/admin/deletions/sweep'), 'sweep ran')">Sweep now</button><button class="btn s p" @click="dlg = 'import'; form = { text: '' }">Import tombstones</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="stats">
    <div class="stat"><div class="k">Tombstones</div><div class="v">{{ (x.d?.deletions ?? []).length }}<small>{{ (x.d?.deletions ?? []).filter((r: any) => r.lifted_at).length }} lifted</small></div></div>
    <div class="stat"><div class="k">Re-applied</div><div class="v">{{ (x.d?.deletions ?? []).reduce((a: number, r: any) => a + (r.reapplied ?? 0), 0) }}<small>times data came back and was removed again</small></div></div>
    <div class="stat"><div class="k">Sources</div><div class="v">{{ [...new Set((x.d?.deletions ?? []).map((r: any) => r.source))].join(', ') || '—' }}</div></div>
  </div>
  <div class="card">
    <div class="tools"><input class="in" v-model="q" placeholder="account id or reason" /><button class="btn" @click="load">Refresh</button></div>
    <DataTable :cols="[{ key: 'account_id', mono: true }, { key: 'deleted_at', fmt: (r) => ts(r.deleted_at) }, { key: 'source' }, { key: 'reason', dim: true }, { key: 'reapplied', fmt: (r) => r.reapplied ? `${r.reapplied}× (last ${ts(r.last_reapplied_at)})` : 'never' }, { key: 'lifted_at', label: 'lifted', fmt: (r) => r.lifted_at ? `${ts(r.lifted_at)} by ${r.lifted_by ?? '?'}` : '' }]" :rows="rows()" empty="No deletions recorded">
      <template #actions="{ row }"><button class="btn s d" v-if="!row.lifted_at" @click="dlg = 'lift'; form = { id: row.account_id, by: '', reason: '' }">Lift</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">A tombstone outlives the account: imports, restores from export and late writes for that id are refused and swept. Lift one only to deliberately re-create the account (the lift is audited with who and why).</p>
  </div>
  <Drawer v-if="dlg === 'import'" title="Import tombstones" @close="dlg = ''"><div class="form">
    <p class="note">One account id per line, optionally <code>, reason</code> — e.g. deletions carried over from a previous backend.</p>
    <div class="f"><textarea class="in" v-model="form.text" style="min-height:160px" placeholder="01a1…, GDPR request&#10;01a2…"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.text?.trim()" @click="doImport">Import</button></div>
    <JsonView v-if="x.imp" :value="x.imp" />
  </div></Drawer>
  <Drawer v-if="dlg === 'lift'" title="Lift tombstone" @close="dlg = ''"><div class="form">
    <p class="note">Account <code>{{ form.id }}</code> may be created again afterwards. Both fields go to the audit log.</p>
    <div class="f"><label>Lifted by</label><input class="in" v-model="form.by" placeholder="your name / ticket" /></div>
    <div class="f"><label>Reason</label><input class="in" v-model="form.reason" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn d" :disabled="!form.by || !form.reason" @click="act(() => post(`/v1/admin/deletions/${form.id}/lift`, { by: form.by, reason: form.reason }), 'lifted')">Lift</button></div>
  </div></Drawer>
</template>
