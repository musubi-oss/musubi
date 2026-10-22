<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, onMounted, watch, computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id))
const A = computed(() => `/v1/admin/accounts/${id.value}`), S = computed(() => `/v1/server/accounts/${id.value}`)
const acct = ref<any>(null); const err = ref(''); const tab = ref(String(route.query.tab ?? 'account'))
const x = ref<Record<string, any>>({})
const tabs: [string, string][] = [['account', 'account'], ['wallet', 'wallet & inventory'], ['heroes', 'heroes'], ['storage', 'storage'], ['social', 'social'], ['sessions', 'sessions & devices'], ['bans', 'bans & reports'], ['security', 'security'], ['notes', 'notes'], ['timeline', 'timeline']]
const dlg = ref<'' | 'ban' | 'kick' | 'note' | 'sanction' | 'edit' | 'limits'>('')
const form = ref<any>({})
async function load() { err.value = ''; x.value = {}; try { acct.value = await get(A.value) } catch (e: any) { err.value = e.message } }
async function fetchInto(key: string, path: string, force = false) {
  if (!force && x.value[key] !== undefined) return
  try { x.value[key] = await get(path) } catch (e: any) { x.value[key] = e.status === 404 ? null : { error: e.message } }
}
function loadTab(t: string, force = false) {
  const a = A.value, s = S.value
  if (t === 'account') { fetchInto('geo', `${s}/geo`, force); fetchInto('presence', `/v1/server/presence/${id.value}`, force); fetchInto('experiments', `/v1/server/experiments/of/${id.value}`, force); fetchInto('audiences', `/v1/server/audiences/of/${id.value}`, force); fetchInto('attribution', `${s}/attribution`, force) }
  if (t === 'wallet') { fetchInto('wallet', `${s}/wallet`, force); fetchInto('inventory', `${s}/inventory`, force); fetchInto('ledger', `${s}/ledger?limit=100`, force); fetchInto('grants', `${a}/grants`, force); fetchInto('limits', `${a}/limits`, force); fetchInto('vip', `${s}/vip`, force); fetchInto('entitlements', `${s}/entitlements`, force) }
  if (t === 'heroes') { fetchInto('heroes', `${s}/heroes`, force); fetchInto('stats', `/v1/server/stats/${id.value}`, force); fetchInto('effects', `/v1/server/effects/${id.value}`, force); fetchInto('tutorials', `${s}/tutorials`, force) }
  if (t === 'storage') { fetchInto('files', `${s}/files`, force); fetchInto('baggage', `${s}/baggage`, force); fetchInto('event_properties', `${s}/event_properties`, force); fetchInto('clock', `${s}/clock`, force) }
  if (t === 'social') { fetchInto('privacy', `${s}/privacy`, force); fetchInto('referrals', `${s}/referrals`, force); fetchInto('donations', `${s}/donations`, force); fetchInto('steam', `${a}/steam`, force); fetchInto('developer', `${a}/developer`, force) }
  if (t === 'sessions') { fetchInto('sessions', `${a}/sessions`, force); fetchInto('devices', `${a}/devices`, force); fetchInto('ips', `${a}/ips`, force) }
  if (t === 'bans') { fetchInto('ban', `${a}/ban`, force); fetchInto('sanctions', `${a}/sanctions`, force); fetchInto('reports', `${a}/reports`, force); fetchInto('anticheat', `${a}/anticheat`, force); fetchInto('crashes', `${a}/crashes`, force); fetchInto('fraud', `/v1/server/economy/fraud/${id.value}`, force) }
  if (t === 'security') { fetchInto('security', `${a}/security_events?limit=100`, force); fetchInto('consents', `${a}/consents`, force) }
  if (t === 'notes') fetchInto('notes', `${a}/notes`, force)
  if (t === 'timeline') fetchInto('timeline', `${a}/timeline?limit=200`, force)
}
watch(tab, (t) => { router.replace({ query: { ...route.query, tab: t } }); loadTab(t) }, { immediate: true })
watch(id, () => { load(); loadTab(tab.value, true) })
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; await load(); loadTab(tab.value, true) } catch (e: any) { toast.err(e) } }
function openDlg(k: typeof dlg.value) {
  dlg.value = k
  form.value = k === 'ban' ? { reason: '', reason_code: '', days: '', shadow: false, message: '', devices: false }
    : k === 'kick' ? { reason: 'kicked by operator' }
    : k === 'note' ? { text: '' }
    : k === 'sanction' ? { kind: 'mute', reason: '', until: '', scope: '' }
    : k === 'limits' ? JSON.stringify(x.value.limits ?? { daily_grant: null }, null, 2)
    : { display_name: acct.value?.account?.display_name ?? '', lang: acct.value?.account?.lang ?? '', metadata: acct.value?.account?.metadata ?? {} }
}
const online = computed(() => (acct.value?.online?.length ?? 0) > 0)
const status = computed(() => { const s = acct.value?.status ?? {}; return s.deleted_at ? 'deleted' : s.disabled_at ? 'disabled' : x.value.ban ? 'banned' : 'active' })
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
const ask = (m: string) => window.confirm(m)
async function exportJson() {
  try { const r: Response = await api('GET', `${A.value}/export`, undefined, { raw: true }); const b = await r.blob(); const u = URL.createObjectURL(b); const el = document.createElement('a'); el.href = u; el.download = `${id.value}.json`; el.click(); toast.show('exported') } catch (e: any) { toast.err(e) }
}
const list = (k: string, sub?: string) => { const v = x.value[k]; if (!v || v.error) return []; const a = sub ? v[sub] : Array.isArray(v) ? v : Object.values(v).find(Array.isArray); return Array.isArray(a) ? a : [] }
const pg2 = usePager(() => list('notes'))
const pg1 = usePager(() => list('timeline', 'items'))
</script>
<template>
  <div class="crumb"><router-link to="/players">Accounts</router-link> › <span>{{ acct?.account?.display_name || acct?.account?.username || id }}</span><span class="id">{{ id }}</span>
    <span class="act" style="margin-left:auto;display:flex;gap:8px">
      <button class="btn s" @click="openDlg('edit')">Edit</button>
      <button class="btn s" @click="openDlg('note')">Add note</button>
      <button class="btn s" :disabled="!online" @click="openDlg('kick')">Kick</button>
      <button class="btn s" v-if="!x.ban" @click="openDlg('ban')">Ban</button>
      <button class="btn s" v-else @click="act(() => post(`${A}/unban`), 'unbanned')">Unban</button>
      <button class="btn s" @click="openDlg('sanction')">Sanction</button>
      <button class="btn s" @click="exportJson">Export</button>
      <button class="btn s d" v-if="!acct?.status?.deleted_at" @click="ask('Soft-delete this account? Data rights erasure runs on the retention schedule.') && act(() => api('DELETE', A), 'deleted')">Delete</button>
      <button class="btn s" v-else @click="act(() => post(`${A}/restore`), 'restored')">Restore</button>
    </span>
  </div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="acct">
    <div class="tabs"><a v-for="[t, label] in tabs" :key="t" :class="{ on: tab === t }" @click="tab = t">{{ label }}</a></div>

    <template v-if="tab === 'account'">
      <div class="grid">
        <div><Kv k="Account ID" :v="acct.account.id" mono copy /><Kv k="Username" :v="acct.account.username" copy /><Kv k="Display name" :v="acct.account.display_name" /><Kv k="Status"><span :class="{ bad: status !== 'active' }">{{ status }}</span><span class="dim3" v-if="online"> · online ({{ acct.online.length }})</span></Kv></div>
        <div><Kv k="Created" :v="ts(acct.account.created_at)" /><Kv k="Updated" :v="ts(acct.account.updated_at)" /><Kv k="Language" :v="acct.account.lang" /><Kv k="Timezone" :v="acct.account.timezone" /></div>
        <div><Kv k="Country" :v="x.geo?.country" /><Kv k="City" :v="x.geo?.city ? `${x.geo.city}${x.geo.region ? ', ' + x.geo.region : ''}` : undefined" /><Kv k="Last IP" :v="x.geo?.ip" mono copy /><Kv k="Location changes" :v="x.geo?.changes" /></div>
      </div>
      <h3>Identities <small>({{ acct.account.identities?.length ?? 0 }})</small></h3>
      <table><thead><tr><th style="width:160px">Provider</th><th>Subject</th><th style="width:90px">Verified</th><th style="width:60px"></th></tr></thead><tbody>
        <tr v-for="i in acct.account.identities ?? []" :key="i.provider + i.subject"><td>{{ i.provider }}</td><td class="m">{{ i.subject }}</td><td class="dim">{{ i.verified ? 'yes' : 'no' }}</td><td><button class="btn s d" @click="ask(`Unlink ${i.provider}?`) && act(() => api('DELETE', `${A}/identities/${encodeURIComponent(i.provider)}/${encodeURIComponent(i.subject)}`), 'unlinked')">Unlink</button></td></tr>
        <tr v-if="!acct.account.identities?.length"><td colspan="4" class="empty">No Data</td></tr></tbody></table>
      <h3>Experiments <small>({{ list('experiments').length }})</small></h3>
      <DataTable :cols="[{ key: 'experiment_id', label: 'experiment', mono: true }, { key: 'variant' }, { key: 'assigned_at', label: 'assigned', dim: true }]" :rows="list('experiments')" />
      <h3>Audiences <small>({{ list('audiences').length }})</small></h3>
      <div class="dim" v-if="!list('audiences').length">No Data</div>
      <div v-else>{{ list('audiences').map((a: any) => a.name ?? a).join(', ') }}</div>
      <h3>Attribution</h3><JsonView v-if="x.attribution" :value="x.attribution" /><div class="dim" v-else>No Data</div>
      <h3>Metadata</h3><JsonView :value="acct.account.metadata ?? {}" />
    </template>

    <template v-else-if="tab === 'wallet'">
      <h3 style="margin-top:0">Wallet</h3>
      <DataTable :cols="[{ key: 'currency', mono: true }, { key: 'balance' }]" :rows="list('wallet', 'balances')" />
      <h3>Ledger <small>(last {{ list('ledger', 'entries').length }})</small></h3>
      <DataTable :cols="[{ key: 'created_at', label: 'at', fmt: (r) => ts(r.created_at ?? r.at), dim: true, width: '170px' }, { key: 'currency', mono: true }, { key: 'delta', fmt: (r) => r.delta ?? r.amount }, { key: 'balance_after', label: 'balance' }, { key: 'reason' }, { key: 'ref', mono: true }, { key: 'metadata', fmt: (r) => Object.keys(r.metadata ?? {}).length ? JSON.stringify(r.metadata) : '—', dim: true }]" :rows="list('ledger', 'entries')" />
      <h3>Inventory <small>({{ list('inventory').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'item_id', label: 'item', mono: true }, { key: 'quantity', label: 'qty' }, { key: 'slot' }, { key: 'bound' }, { key: 'item_version', label: 'version' }, { key: 'instance', fmt: (r) => Object.keys(r.instance ?? {}).length ? JSON.stringify(r.instance) : '—', dim: true }]" :rows="list('inventory', 'items')" />
      <h3>Entitlements</h3><DataTable :cols="[{ key: 'provider' }, { key: 'external_id', mono: true }, { key: 'source' }, { key: 'active' }, { key: 'granted_at', fmt: (r) => ts(r.granted_at), dim: true }, { key: 'expires_at', fmt: (r) => r.expires_at ? ts(r.expires_at) : '—', dim: true }, { key: 'revoked_at', fmt: (r) => r.revoked_at ? ts(r.revoked_at) : '—', dim: true }]" :rows="list('entitlements', 'entitlements')" />
      <h3>VIP programs</h3><DataTable :cols="[{ key: 'program' }, { key: 'tier' }, { key: 'points' }, { key: 'expires_at', dim: true }]" :rows="list('vip', 'programs')" />
      <h3>Grants <small>({{ list('grants').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'amount' }, { key: 'actor' }, { key: 'reason' }, { key: 'created_at', dim: true }]" :rows="list('grants')" />
      <h3>Grant limit override <button class="btn s" style="margin-left:8px" @click="openDlg('limits')">Edit</button><button class="btn s d" v-if="x.limits" style="margin-left:6px" @click="act(() => api('DELETE', `${A}/limits`), 'limit override removed')">Remove</button></h3>
      <JsonView v-if="x.limits" :value="x.limits" /><div class="dim" v-else>No override — global limits apply</div>
    </template>

    <template v-else-if="tab === 'heroes'">
      <h3 style="margin-top:0">Heroes <small>({{ list('heroes').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'class_id', label: 'class' }, { key: 'name' }, { key: 'level' }, { key: 'loadout' }, { key: 'created_at', dim: true }]" :rows="list('heroes')" />
      <h3>Stats</h3><DataTable :cols="[{ key: 'stat', mono: true }, { key: 'value' }, { key: 'updated_at', dim: true }]" :rows="list('stats')" />
      <h3>Active effects</h3><DataTable :cols="[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'value' }, { key: 'expires_at', dim: true }]" :rows="list('effects')" />
      <h3>Tutorials</h3><JsonView v-if="x.tutorials" :value="x.tutorials" /><div class="dim" v-else>No Data</div>
    </template>

    <template v-else-if="tab === 'storage'">
      <h3 style="margin-top:0">Files</h3><DataTable :cols="[{ key: 'key', mono: true }, { key: 'size' }, { key: 'mime' }, { key: 'updated_at', dim: true }]" :rows="list('files')" />
      <h3>Baggage</h3><JsonView v-if="x.baggage" :value="x.baggage" /><div class="dim" v-else>No Data</div>
      <h3>Event properties</h3><JsonView v-if="x.event_properties" :value="x.event_properties" /><div class="dim" v-else>No Data</div>
      <h3>Clock</h3><JsonView v-if="x.clock" :value="x.clock" /><div class="dim" v-else>No Data</div>
    </template>

    <template v-else-if="tab === 'social'">
      <div class="grid"><div><Kv k="Profile visibility" :v="x.privacy?.profile" /></div><div><Kv k="Inventory visibility" :v="x.privacy?.inventory" /></div><div><Kv k="Show online" :v="x.privacy?.show_online" /></div></div>
      <h3>Referrals</h3><JsonView v-if="x.referrals" :value="x.referrals" /><div class="dim" v-else>No Data</div>
      <h3>Donations</h3><DataTable :cols="[{ key: 'id', mono: true }, { key: 'from' }, { key: 'amount' }, { key: 'currency' }, { key: 'created_at', dim: true }]" :rows="list('donations')" />
      <h3>Steam</h3><JsonView v-if="x.steam" :value="x.steam" /><div class="dim" v-else>Not linked</div>
      <h3>Developer</h3><JsonView v-if="x.developer" :value="x.developer" /><div class="dim" v-else>Not a developer account</div>
    </template>

    <template v-else-if="tab === 'sessions'">
      <h3 style="margin-top:0">Sessions <small>({{ list('sessions', 'sessions').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'device_id', mono: true }, { key: 'ip', mono: true }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'last_seen_at', label: 'last seen', fmt: (r) => ts(r.last_seen_at), dim: true }, { key: 'expires_at', fmt: (r) => ts(r.expires_at), dim: true }, { key: 'revoked_at', label: 'revoked', fmt: (r) => r.revoked_at ? ts(r.revoked_at) : '—' }]" :rows="list('sessions', 'sessions')" />
      <h3>Devices <small>({{ list('devices').length }})</small></h3>
      <DataTable :cols="[{ key: 'device_id', mono: true }, { key: 'logins' }, { key: 'banned' }, { key: 'last_ip', mono: true }, { key: 'user_agent', dim: true }, { key: 'first_seen_at', fmt: (r) => ts(r.first_seen_at), dim: true }, { key: 'last_seen_at', fmt: (r) => ts(r.last_seen_at), dim: true }]" :rows="list('devices')" />
      <div class="tools" style="margin-top:8px"><button class="btn s d" v-if="list('devices').length" @click="ask('Ban all devices of this account?') && act(() => post(`${A}/ban/devices`, { reason: 'operator' }), 'devices banned')">Ban all devices</button></div>
      <h3>IP history <small>({{ list('ips').length }})</small></h3>
      <DataTable :cols="[{ key: 'ip', mono: true }, { key: 'country' }, { key: 'logins' }, { key: 'first_seen_at', fmt: (r) => ts(r.first_seen_at), dim: true }, { key: 'last_seen_at', fmt: (r) => ts(r.last_seen_at), dim: true }]" :rows="list('ips')" />
    </template>

    <template v-else-if="tab === 'bans'">
      <h3 style="margin-top:0">Ban</h3>
      <div v-if="x.ban && !x.ban.error" class="grid"><div><Kv k="Reason" :v="x.ban.reason" /><Kv k="Code" :v="x.ban.reason_code" /></div><div><Kv k="Until" :v="x.ban.until ? ts(x.ban.until) : 'permanent'" /><Kv k="Shadow" :v="x.ban.shadow" /></div><div><Kv k="Actor" :v="x.ban.actor" /><Kv k="Since" :v="ts(x.ban.created_at)" /></div></div>
      <div class="dim" v-else>Not banned</div>
      <h3>Sanctions <small>({{ list('sanctions').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'scope' }, { key: 'reason' }, { key: 'until', fmt: (r) => r.until ? ts(r.until) : 'permanent' }, { key: 'actor' }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="list('sanctions')" />
      <h3>Reports about this player <small>({{ list('reports').length }})</small></h3>
      <DataTable :cols="[{ key: 'id', mono: true }, { key: 'category' }, { key: 'reporter_id', mono: true }, { key: 'status' }, { key: 'decision' }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="list('reports')" pick @pick="(r) => router.push(`/players/reports`)" />
      <h3>Anti-cheat</h3><JsonView v-if="x.anticheat" :value="x.anticheat" /><div class="dim" v-else>No Data</div>
      <h3>Economy fraud</h3><JsonView v-if="x.fraud" :value="x.fraud" /><div class="dim" v-else>No flags</div>
      <h3>Crashes</h3><DataTable :cols="[{ key: 'fingerprint', mono: true }, { key: 'version' }, { key: 'at', fmt: (r) => ts(r.at ?? r.created_at), dim: true }]" :rows="list('crashes')" />
    </template>

    <template v-else-if="tab === 'security'">
      <h3 style="margin-top:0">Security events</h3>
      <DataTable :cols="[{ key: 'at', fmt: (r) => ts(r.at), dim: true, width: '170px' }, { key: 'kind' }, { key: 'ip', mono: true }, { key: 'summary', fmt: (r) => r.summary ?? JSON.stringify(r.data ?? r.payload ?? {}) }]" :rows="list('security')" />
      <h3>Consents <small>({{ list('consents', 'consents').length }})</small></h3>
      <DataTable :cols="[{ key: 'policy' }, { key: 'version' }, { key: 'granted' }, { key: 'at', fmt: (r) => ts(r.at ?? r.granted_at), dim: true }]" :rows="list('consents', 'consents')" />
      <div class="dim" v-if="list('consents', 'pending').length">Pending: {{ list('consents', 'pending').map((p: any) => p.policy ?? p).join(', ') }}</div>
    </template>

    <template v-else-if="tab === 'notes'">
      <div class="tools"><button class="btn p" @click="openDlg('note')">Add note</button></div>
      <table><thead><tr><th style="width:170px">At</th><th style="width:160px">Author</th><th>Note</th><th style="width:60px"></th></tr></thead><tbody>
        <tr v-for="n in pg2.rows" :key="n.id"><td class="dim">{{ ts(n.created_at) }}</td><td>{{ n.author ?? n.actor }}</td><td style="white-space:pre-wrap">{{ n.text ?? n.body }}</td><td><button class="btn s d" @click="act(() => api('DELETE', `${A}/notes/${n.id}`), 'note deleted')">Delete</button></td></tr>
        <tr v-if="!pg2.total"><td colspan="4" class="empty">No Data</td></tr></tbody></table><Pager :p="pg2" />
    </template>

    <template v-else-if="tab === 'timeline'">
      <table><thead><tr><th style="width:170px">At</th><th style="width:220px">Kind</th><th style="width:90px">Source</th><th>Summary</th></tr></thead><tbody>
        <tr v-for="(it, i) in pg1.rows" :key="i"><td class="dim">{{ ts(it.at) }}</td><td class="m">{{ it.kind }}</td><td class="dim">{{ it.source }}</td><td class="m dim" style="white-space:pre-wrap;word-break:break-all">{{ it.summary }}</td></tr>
        <tr v-if="!list('timeline', 'items').length"><td colspan="4" class="empty">No Data</td></tr></tbody></table><Pager :p="pg1" />
    </template>
  </div>

  <Drawer v-if="dlg === 'ban'" title="Ban player" @close="dlg = ''"><div class="form">
    <div class="f"><label>Reason (shown to the player)</label><input class="in" v-model="form.reason" /></div>
    <div class="f"><label>Reason code</label><input class="in" v-model="form.reason_code" placeholder="cheating, toxicity, …" /></div>
    <div class="f"><label>Duration, days (empty = permanent)</label><input class="in" v-model="form.days" type="number" min="0" step="0.5" /></div>
    <div class="f"><label>Internal message</label><input class="in" v-model="form.message" /></div>
    <div class="f"><label><input type="checkbox" v-model="form.shadow" /> shadow ban (player is not told)</label></div>
    <div class="f"><label><input type="checkbox" v-model="form.devices" /> also ban known devices</label></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(async () => { const b: any = { reason: form.reason, reason_code: form.reason_code || undefined, until_secs: form.days ? Math.round(Number(form.days) * 86400) : undefined, shadow: form.shadow, message: form.message || undefined }; await post(`${A}/ban`, b); if (form.devices) await post(`${A}/ban/devices`, { reason: form.reason }) }, 'banned')">Ban</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'kick'" title="Kick player" @close="dlg = ''"><div class="form">
    <div class="f"><label>Reason</label><input class="in" v-model="form.reason" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${A}/kick`, { reason: form.reason }), 'kicked')">Kick</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'note'" title="Add note" @close="dlg = ''"><div class="form">
    <div class="f"><label>Note</label><textarea class="in" v-model="form.text" style="min-height:120px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.text" @click="act(() => post(`${A}/notes`, { text: form.text }), 'note added')">Add</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'sanction'" title="Sanction" @close="dlg = ''"><div class="form">
    <div class="f"><label>Kind</label><select class="sel" v-model="form.kind"><option>mute</option><option>chat_restricted</option><option>matchmaking_restricted</option><option>trade_restricted</option><option>ugc_restricted</option><option>warning</option></select></div>
    <div class="f"><label>Scope (optional)</label><input class="in" v-model="form.scope" placeholder="channel, queue, …" /></div>
    <div class="f"><label>Reason</label><input class="in" v-model="form.reason" /></div>
    <div class="f"><label>Until (ISO, empty = permanent)</label><input class="in" v-model="form.until" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${A}/sanctions`, { kind: form.kind, scope: form.scope || undefined, reason: form.reason, until: form.until || undefined }), 'sanctioned')">Apply</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'edit'" title="Edit account" @close="dlg = ''"><div class="form">
    <div class="f"><label>Display name</label><input class="in" v-model="form.display_name" /></div>
    <div class="f"><label>Language</label><input class="in" v-model="form.lang" /></div>
    <div class="f"><label>Metadata (JSON)</label><textarea class="in" :value="JSON.stringify(form.metadata, null, 2)" @input="(e: any) => { try { form.metadata = JSON.parse(e.target.value) } catch {} }"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => api('PATCH', A, { display_name: form.display_name || null, lang: form.lang || undefined, metadata: form.metadata }), 'saved')">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'limits'" title="Grant limit override" @close="dlg = ''"><div class="form">
    <div class="f"><label>Limits (JSON)</label><textarea class="in" v-model="form" style="min-height:200px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => api('PUT', `${A}/limits`, JSON.parse(form)), 'limits saved')">Save</button></div>
  </div></Drawer>
</template>
