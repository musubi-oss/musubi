<script setup lang="ts">
// The sanction ladder: proposals awaiting a second pair of eyes, active bans/mutes/restrictions,
// appeals to decide, and the reason catalogue that gives every sanction a code and a message.
import { ref, onMounted } from 'vue'
import { api, get, post, put, qs } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const status = ref(''); const action = ref(''); const appealStatus = ref('open'); const dlg = ref<'' | 'reason' | 'note'>(''); const form = ref<any>({})
async function load() { err.value = ''; const r = await Promise.allSettled([get('/v1/admin/sanctions' + qs({ status: status.value || undefined, action: action.value || undefined, limit: 300 })), get('/v1/admin/appeals' + qs({ status: appealStatus.value || undefined, limit: 200 })), get('/v1/admin/bans/reasons')]); const v = (i: number) => r[i].status === 'fulfilled' ? (r[i] as any).value : (err.value += (r[i] as any).reason?.message + ' ', null); x.value.s = v(0); x.value.a = v(1); x.value.r = v(2) }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
const dur = (r: any) => !r.ends_at ? 'permanent' : r.ends_at - (r.starts_at ?? r.created_at) < 86400 ? Math.round((r.ends_at - (r.starts_at ?? r.created_at)) / 3600) + ' h' : Math.round((r.ends_at - (r.starts_at ?? r.created_at)) / 86400) + ' d'
function withNote(title: string, path: string, ok: string, danger = false) { dlg.value = 'note'; form.value = { title, path, ok, danger, note: '' } }
function decide(id: string, outcome: 'upheld' | 'overturned') { dlg.value = 'note'; form.value = { title: `Appeal ${outcome}`, path: `/v1/admin/appeals/${id}/decide`, ok: `appeal ${outcome}`, danger: outcome === 'upheld', note: '', body: { outcome } } }
function openReason(r?: any) { dlg.value = 'reason'; form.value = r ? { ...r, features: (r.features ?? []).join(', '), default_duration_secs: r.default_duration_secs ?? '' } : { code: '', title: '', message: '', severity: 'medium', default_duration_secs: '', features: '' } }
const counts = () => { const s = x.value.s?.sanctions ?? []; return { pending: s.filter((r: any) => r.status === 'pending').length, active: s.filter((r: any) => r.status === 'active').length } }
</script>
<template>
  <div class="crumb"><span>Sanctions &amp; appeals</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="openReason()">Add reason code</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="stats">
    <div class="stat"><div class="k">Awaiting approval</div><div class="v" :class="{ warn: counts().pending }">{{ counts().pending }}<small>proposed, not yet in force</small></div></div>
    <div class="stat"><div class="k">Active</div><div class="v">{{ counts().active }}</div></div>
    <div class="stat"><div class="k">Open appeals</div><div class="v" :class="{ warn: (x.a?.appeals ?? []).length && appealStatus === 'open' }">{{ appealStatus === 'open' ? (x.a?.appeals ?? []).length : '—' }}</div></div>
    <div class="stat"><div class="k">Reason codes</div><div class="v">{{ (x.r?.reasons ?? []).length }}</div></div>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Sanctions <small>({{ (x.s?.sanctions ?? []).length }})</small></h3>
    <div class="tools"><select class="in" v-model="status" @change="load"><option value="">any status</option><option v-for="s in ['pending', 'active', 'expired', 'revoked', 'rejected']" :key="s" :value="s">{{ s }}</option></select><select class="in" v-model="action" @change="load"><option value="">any action</option><option v-for="a in ['ban', 'mute', 'restrict', 'shadow', 'warn']" :key="a" :value="a">{{ a }}</option></select><button class="btn" @click="load">Refresh</button></div>
    <DataTable :cols="[{ key: 'status' }, { key: 'action' }, { key: 'account_id', mono: true }, { key: 'reason_code', fmt: (r) => r.reason_code ?? r.reason ?? '—' }, { key: 'message', dim: true }, { key: 'features', fmt: (r) => (r.features ?? []).join(' ') }, { key: 'dur', label: 'duration', fmt: dur }, { key: 'created_by', fmt: (r) => `${r.created_by} · ${ts(r.created_at)}`, dim: true }, { key: 'revoked_at', label: 'revoked', fmt: (r) => r.revoked_at ? `${ts(r.revoked_at)} ${r.revoke_note ?? ''}` : '', dim: true }]" :rows="x.s?.sanctions ?? []" pick @pick="(r) => (x.pick = r)" empty="No sanctions match">
      <template #actions="{ row }">
        <template v-if="row.status === 'pending'"><button class="btn s p" @click="withNote('Approve sanction', `/v1/admin/sanctions/${row.id}/approve`, 'approved')">Approve</button><button class="btn s" @click="withNote('Reject sanction', `/v1/admin/sanctions/${row.id}/reject`, 'rejected')">Reject</button></template>
        <button class="btn s d" v-else-if="row.status === 'active'" @click="withNote('Revoke sanction', `/v1/admin/sanctions/${row.id}/revoke`, 'revoked', true)">Revoke</button>
      </template>
    </DataTable>
    <p class="note" style="margin-top:10px">A sanction proposed by a support key or by anti-cheat stays <code>pending</code> until an admin approves it (four-eyes); <code>active</code> ones are enforced at sign-in and in chat; revoking keeps the record with your note.</p>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Appeals <small>({{ (x.a?.appeals ?? []).length }})</small></h3>
    <div class="tools"><select class="in" v-model="appealStatus" @change="load"><option value="open">open</option><option value="upheld">upheld</option><option value="overturned">overturned</option><option value="">all</option></select></div>
    <DataTable :cols="[{ key: 'status' }, { key: 'account_id', mono: true }, { key: 'sanction_id', mono: true, dim: true }, { key: 'text', fmt: (r) => r.text ?? r.body ?? '' }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'decision_note', fmt: (r) => r.decision_note ? `${r.decision_note} (${r.decided_by ?? '?'}, ${ts(r.decided_at)})` : '', dim: true }]" :rows="x.a?.appeals ?? []" empty="No appeals">
      <template #actions="{ row }"><template v-if="row.status === 'open'"><button class="btn s" @click="decide(row.id, 'overturned')">Overturn</button><button class="btn s d" @click="decide(row.id, 'upheld')">Uphold</button></template></template>
    </DataTable>
    <p class="note" style="margin-top:10px">Overturning revokes the sanction; upholding keeps it and closes the appeal. The player sees the note.</p>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Reason codes <small>({{ (x.r?.reasons ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'code', mono: true }, { key: 'title' }, { key: 'severity' }, { key: 'message', dim: true }, { key: 'default_duration_secs', label: 'default duration', fmt: (r) => r.default_duration_secs ? Math.round(r.default_duration_secs / 3600) + ' h' : 'permanent / none' }, { key: 'features', fmt: (r) => (r.features ?? []).join(' ') || 'whole account' }]" :rows="x.r?.reasons ?? []" empty="No reason codes — sanctions then carry free text only">
      <template #actions="{ row }"><button class="btn s" @click="openReason(row)">Edit</button><button class="btn s d" @click="ask(`Delete reason ${row.code}?`) && act(() => api('DELETE', `/v1/admin/bans/reasons/${encodeURIComponent(row.code)}`), 'deleted')">Delete</button></template>
    </DataTable>
  </div>
  <Drawer v-if="x.pick" title="Sanction" @close="x.pick = null"><JsonView :value="x.pick" /></Drawer>
  <Drawer v-if="dlg === 'note'" :title="form.title" @close="dlg = ''"><div class="form">
    <div class="f"><label>Note (kept on the record{{ form.body ? ', shown to the player' : '' }})</label><textarea class="in" v-model="form.note" style="min-height:80px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn" :class="form.danger ? 'd' : 'p'" @click="act(() => post(form.path, { ...(form.body ?? {}), note: form.note }), form.ok)">{{ form.title.split(' ')[0] }}</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'reason'" :title="form.updated_at ? form.code : 'New reason code'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Code</label><input class="in" v-model="form.code" placeholder="cheating" :disabled="!!form.updated_at" /></div>
    <div class="f"><label>Title</label><input class="in" v-model="form.title" /></div>
    <div class="f"><label>Message to the player</label><textarea class="in" v-model="form.message" style="min-height:60px"></textarea></div>
    <div class="f"><label>Severity</label><select class="in" v-model="form.severity"><option v-for="s in ['low', 'medium', 'high', 'critical']" :key="s" :value="s">{{ s }}</option></select></div>
    <div class="f"><label>Default duration (seconds, empty = permanent)</label><input class="in" type="number" v-model="form.default_duration_secs" /></div>
    <div class="f"><label>Features restricted (comma-separated, empty = whole account)</label><input class="in" v-model="form.features" placeholder="chat, trading" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.code || !form.title" @click="act(() => put(`/v1/admin/bans/reasons/${encodeURIComponent(form.code)}`, { title: form.title, message: form.message, severity: form.severity, default_duration_secs: form.default_duration_secs === '' ? null : Number(form.default_duration_secs), features: String(form.features).split(',').map((s: string) => s.trim()).filter(Boolean) }), 'saved')">Save</button></div>
  </div></Drawer>
</template>
