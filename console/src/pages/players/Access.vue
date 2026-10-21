<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const pol = ref<any>(null); const eff = ref<any>(null); const allow = ref<any[]>([]); const denials = ref<any[]>([]); const queue = ref<any>(null); const err = ref(''); const dlg = ref(''); const form = ref<any>({}); const windows = ref<{ from: string; to: string }[]>([]); const bypassA = ref(''); const bypassP = ref(''); const provs = ref('')
const toIso = (v: any) => v ? new Date(typeof v === 'number' ? v * 1000 : v).toISOString().slice(0, 16) : ''
async function load() {
  err.value = ''
  const r = await Promise.allSettled([get('/v1/admin/access/policy'), get('/v1/admin/access/allowlist'), get('/v1/admin/access/denials'), get('/v1/admin/auth/queue')])
  const v = (i: number) => (r[i].status === 'fulfilled' ? (r[i] as any).value : (err.value += (r[i] as any).reason?.message + ' ', null))
  const p = v(0); pol.value = p?.policy ?? {}; eff.value = p?.effective ?? null; allow.value = v(1)?.entries ?? []; denials.value = v(2)?.denials ?? []; queue.value = v(3)
  windows.value = (pol.value.windows ?? []).map((w: any) => ({ from: toIso(w.from ?? w.starts_at), to: toIso(w.to ?? w.ends_at) })); bypassA.value = (pol.value.bypass_audiences ?? []).join(', '); bypassP.value = (pol.value.bypass_providers ?? []).join(', '); provs.value = (pol.value.providers ?? []).join(', ')
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
const list = (s: string) => s.split(',').map((x) => x.trim()).filter(Boolean)
async function savePolicy() { await act(() => put('/v1/admin/access/policy', { ...pol.value, windows: windows.value.filter((w) => w.from && w.to).map((w) => ({ from: Math.floor(new Date(w.from).getTime() / 1000), to: Math.floor(new Date(w.to).getTime() / 1000) })), bypass_audiences: list(bypassA.value), bypass_providers: list(bypassP.value), providers: list(provs.value), message: pol.value.message || null, retry_after_secs: pol.value.retry_after_secs || null }), 'policy saved') }
const ts = (v: any) => !v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(v)).toISOString().replace('T', ' ').slice(0, 16)
</script>
<template>
  <div class="crumb"><span>Access &amp; login</span><span class="id" v-if="eff">effective: {{ eff.mode }}{{ eff.next_change_at ? ` · changes ${ts(eff.next_change_at)}` : '' }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="ask('Kick everyone the current policy does not admit?') && act(() => post('/v1/admin/access/enforce'), 'enforced')">Enforce now</button><button class="btn s p" @click="savePolicy">Save policy</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="pol">
    <h3>Access policy</h3>
    <div class="grid">
      <div><Kv k="Mode"><select class="sel" v-model="pol.mode"><option value="open">open — anyone may log in</option><option value="allowlist">allowlist — only listed accounts / emails / providers</option><option value="existing">existing — no new signups</option><option value="closed">closed — maintenance, nobody logs in</option></select></Kv><Kv k="Message shown to rejected players"><input class="in" v-model="pol.message" placeholder="Servers are in maintenance until 18:00 UTC" /></Kv><Kv k="Retry after (s)"><input class="in" type="number" min="0" v-model.number="pol.retry_after_secs" placeholder="—" /></Kv></div>
      <div><Kv k="Flags"><label><input type="checkbox" v-model="pol.allow_existing_accounts" /> existing accounts bypass the allowlist</label><br /><label><input type="checkbox" v-model="pol.kick_on_change" /> kick sessions that stop being admitted when the policy changes</label></Kv><Kv k="Bypass audiences (comma-separated)"><input class="in" v-model="bypassA" placeholder="qa, partners" /></Kv><Kv k="Bypass providers / allowed providers"><input class="in" v-model="bypassP" placeholder="steam" /><input class="in" v-model="provs" placeholder="allowed providers (empty = all)" style="margin-top:6px" /></Kv></div>
      <div><Kv k="Windows (policy applies only inside; empty = always)"><div v-for="(w, i) in windows" :key="i" style="display:flex;gap:6px;margin-bottom:6px"><input class="in" type="datetime-local" v-model="w.from" /><input class="in" type="datetime-local" v-model="w.to" /><button class="btn s" @click="windows.splice(i, 1)">×</button></div><button class="btn s" @click="windows.push({ from: '', to: '' })">Add window</button></Kv><Kv k="Updated" :v="ts(pol.updated_at)" /></div>
    </div>
    <h3>Login queue <small>(admission when capacity is reached)</small> <button class="btn s" style="margin-left:8px" @click="ask('Drop every waiting ticket?') && act(() => api('DELETE', '/v1/admin/auth/queue'), 'queue flushed')">Flush</button></h3>
    <div class="grid" v-if="queue"><div><Kv k="Waiting" :v="queue.waiting" /><Kv k="Online / capacity" :v="`${queue.online} / ${queue.capacity || '∞'}`" /></div><div><Kv k="Admit per second" :v="queue.admit_per_sec" /><Kv k="Admitted recently" :v="queue.admitted_recently" /></div><div><Kv k="State" :v="queue.capacity ? (queue.waiting ? 'queueing' : 'flowing') : 'disabled — no capacity cap in [auth.queue]'" /></div></div>
    <h3>Allowlist <small>({{ allow.length }})</small> <button class="btn s p" style="margin-left:8px" @click="form = { kind: 'account', value: '', note: '' }; dlg = 'add'">Add</button> <button class="btn s" @click="form = { kind: 'email', values: '' }; dlg = 'import'">Import list</button></h3>
    <table><thead><tr><th style="width:110px">Kind</th><th>Value</th><th>Note</th><th style="width:140px">Added</th><th style="width:60px"></th></tr></thead><tbody>
      <tr v-for="e in allow" :key="e.kind + e.value"><td>{{ e.kind }}</td><td class="m">{{ e.value }}</td><td class="dim">{{ e.note }}</td><td class="dim">{{ ts(e.created_at ?? e.added_at) }}</td><td><button class="btn s" @click="act(() => api('DELETE', `/v1/admin/access/allowlist/${e.kind}/${encodeURIComponent(e.value)}`), 'removed')">×</button></td></tr>
      <tr v-if="!allow.length"><td colspan="5" class="empty">Empty — in allowlist mode nobody new gets in</td></tr></tbody></table>
    <h3>Recent denials <small>({{ denials.length }})</small></h3>
    <table><thead><tr><th style="width:140px">At</th><th>Account</th><th style="width:100px">Provider</th><th>Reason</th><th style="width:90px">Mode</th><th style="width:130px">IP</th></tr></thead><tbody>
      <tr v-for="(d, i) in denials" :key="i"><td class="dim">{{ ts(d.at) }}</td><td class="m">{{ d.account_id ?? d.subject ?? '—' }}</td><td>{{ d.provider }}</td><td class="dim">{{ d.reason }}</td><td class="dim">{{ d.mode }}</td><td class="m dim">{{ d.ip }}</td></tr>
      <tr v-if="!denials.length"><td colspan="6" class="empty">Nobody has been denied</td></tr></tbody></table>
  </div>
  <Drawer v-if="dlg === 'add'" title="Allowlist entry" @close="dlg = ''"><div class="form"><div class="f"><label>Kind</label><select class="sel" v-model="form.kind"><option value="account">account id</option><option value="email">email</option><option value="email_domain">email domain</option><option value="provider_subject">provider subject</option></select></div><div class="f"><label>Value</label><input class="in m" v-model="form.value" /></div><div class="f"><label>Note</label><input class="in" v-model="form.note" /></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.value" @click="act(() => put(`/v1/admin/access/allowlist/${form.kind}/${encodeURIComponent(form.value)}`, { note: form.note }), 'added')">Add</button></div></div></Drawer>
  <Drawer v-if="dlg === 'import'" title="Import allowlist" @close="dlg = ''"><div class="form"><div class="f"><label>Kind</label><select class="sel" v-model="form.kind"><option value="account">account ids</option><option value="email">emails</option><option value="email_domain">email domains</option></select></div><div class="f"><label>Values (one per line or comma-separated)</label><textarea class="in m" v-model="form.values" style="min-height:160px"></textarea></div><div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post('/v1/admin/access/allowlist/import', { kind: form.kind, values: form.values.split(/[\n,]/).map((s: string) => s.trim()).filter(Boolean) }), 'imported')">Import</button></div></div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
