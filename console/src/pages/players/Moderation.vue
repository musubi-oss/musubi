<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, onMounted } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { api, get, post, qs } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const router = useRouter(); const ask = (m: string) => window.confirm(m)
const tab = ref<'reports' | 'chat' | 'bans' | 'rules'>((['reports', 'chat', 'bans', 'rules'] as const).find((k) => k === useRoute().query.tab) ?? 'reports')
const reports = ref<any[]>([]); const cursor = ref<string | null>(null); const chat = ref<any[]>([]); const bans = ref<any[]>([]); const bansTotal = ref(0); const rules = ref<any[]>([]); const err = ref('')
const rf = ref({ status: 'open', category: '' }); const bf = ref({ q: '', shadow: 'all' }); const dlg = ref(''); const sel = ref<any>(null); const form = ref<any>({})
async function load(more = false) {
  err.value = ''
  try {
    if (tab.value === 'reports') { const r = await get('/v1/admin/reports' + qs({ limit: 100, cursor: more ? cursor.value : undefined, status: rf.value.status || undefined, category: rf.value.category || undefined })); reports.value = more ? [...reports.value, ...(r.reports ?? [])] : r.reports ?? []; cursor.value = r.cursor ?? null }
    if (tab.value === 'chat') { const r = await get('/v1/admin/chat/reports'); chat.value = Array.isArray(r) ? r : r.reports ?? [] }
    if (tab.value === 'bans') { const r = await get('/v1/admin/bans' + qs({ limit: 200, q: bf.value.q || undefined })); bans.value = (r.bans ?? []).filter((b: any) => bf.value.shadow === 'all' || (bf.value.shadow === 'shadow') === !!b.shadow); bansTotal.value = r.total ?? bans.value.length }
    if (tab.value === 'rules') rules.value = (await get('/v1/admin/bans/rules')).rules ?? []
  } catch (x: any) { err.value = x.message }
}
onMounted(() => load())
function go(t: typeof tab.value) { tab.value = t; load() }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
function decide(r: any) { sel.value = r; form.value = { decision: 'warned', note: '', ban: false, reason: r.category ?? '', until_days: 7, shadow: false }; dlg.value = 'decide' }
async function submitDecision() { const b: any = { status: 'resolved', decision: form.value.decision, note: form.value.note }; if (form.value.ban) b.ban = { reason: form.value.reason, until_secs: Number(form.value.until_days) * 86400 || null, shadow: form.value.shadow }; await act(() => post(`/v1/admin/reports/${sel.value.id}`, b), 'report resolved') }
function decideChat(r: any) { sel.value = r; form.value = { action: 'mute', duration_h: 1, note: '' }; dlg.value = 'chat' }
const ts = (v: any) => !v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(v)).toISOString().replace('T', ' ').slice(0, 16)
const short = (s: any) => s ? String(s).slice(0, 8) + '…' : '—'
const pg3 = usePager(() => reports)
const pg2 = usePager(() => chat)
const pg1 = usePager(() => bans)
</script>
<template>
  <div class="crumb"><span>Moderation</span><span class="id">reports · chat reports · bans · automatic rules</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" v-if="tab === 'rules'" @click="form = { kind: 'reports', threshold: 5, window_secs: 86400, action: 'ban', until_secs: 604800, reason: '', shadow: false, enabled: true }; dlg = 'rule'">Add rule</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tabs"><a :class="{ on: tab === 'reports' }" @click="go('reports')">Player reports</a><a :class="{ on: tab === 'chat' }" @click="go('chat')">Chat reports ({{ chat.length || '…' }})</a><a :class="{ on: tab === 'bans' }" @click="go('bans')">Bans</a><a :class="{ on: tab === 'rules' }" @click="go('rules')">Auto-ban rules</a></div>
    <template v-if="tab === 'reports'">
      <div style="display:flex;gap:8px;margin-bottom:10px"><select class="sel" v-model="rf.status" @change="load()"><option value="open">open</option><option value="resolved">resolved</option><option value="dismissed">dismissed</option><option value="">any status</option></select><input class="in" v-model="rf.category" placeholder="category (spam, cheating, …)" @keyup.enter="load()" style="width:220px" /><button class="btn s" @click="load()">Apply</button></div>
      <table><thead><tr><th style="width:140px">Created</th><th>Target</th><th>Reporter</th><th style="width:110px">Category</th><th>Details</th><th style="width:90px">Status</th><th>Decision</th><th style="width:120px"></th></tr></thead><tbody>
        <tr v-for="r in pg3.rows" :key="r.id"><td class="dim">{{ ts(r.created_at) }}</td><td class="m row" @click="router.push(`/players/accounts/${r.target_id}?tab=bans`)">{{ short(r.target_id) }}</td><td class="m dim row" @click="router.push(`/players/accounts/${r.reporter_id}`)">{{ short(r.reporter_id) }}</td><td>{{ r.category }}</td><td class="dim">{{ r.details || (Object.keys(r.evidence ?? {}).length ? 'evidence attached' : '') }}</td><td :class="{ ok: r.status === 'open' }">{{ r.status }}</td><td class="dim">{{ r.decision ?? '' }}{{ r.decided_by ? ` · ${r.decided_by}` : '' }}</td>
          <td><template v-if="r.status === 'open'"><button class="btn s p" @click="decide(r)">Decide</button> <button class="btn s" @click="act(() => post(`/v1/admin/reports/${r.id}`, { status: 'dismissed', decision: 'no evidence' }), 'dismissed')">Dismiss</button></template></td></tr>
        <tr v-if="!reports.length"><td colspan="8" class="empty">No reports match</td></tr></tbody></table><Pager :p="pg3" />
      <div class="row" style="margin-top:8px" v-if="cursor"><button class="btn s" @click="load(true)">Load more</button></div>
    </template>
    <template v-else-if="tab === 'chat'">
      <table><thead><tr><th style="width:140px">Created</th><th>Channel</th><th>Target</th><th>Reporter</th><th style="width:100px">Reason</th><th style="width:90px">Status</th><th>Context</th><th style="width:100px"></th></tr></thead><tbody>
        <tr v-for="r in pg2.rows" :key="r.id"><td class="dim">{{ ts(r.created_at) }}</td><td class="m dim">{{ short(r.channel_id) }}</td><td class="m row" @click="router.push(`/players/accounts/${r.target_id}`)">{{ short(r.target_id) }}</td><td class="m dim">{{ short(r.reporter_id) }}</td><td>{{ r.reason }}</td><td :class="{ ok: (r.status ?? 'open') === 'open' }">{{ r.status ?? 'open' }}</td><td class="dim" style="white-space:pre-wrap">{{ (r.context ?? []).slice(-3).map((m: any) => `${short(m.sender_id ?? m.account_id)}: ${m.content ?? m.text ?? ''}`).join('\n') }}</td><td><button class="btn s p" v-if="(r.status ?? 'open') === 'open'" @click="decideChat(r)">Decide</button></td></tr>
        <tr v-if="!chat.length"><td colspan="8" class="empty">No chat reports</td></tr></tbody></table><Pager :p="pg2" />
    </template>
    <template v-else-if="tab === 'bans'">
      <div style="display:flex;gap:8px;margin-bottom:10px"><input class="in" v-model="bf.q" placeholder="username / account id" @keyup.enter="load()" style="width:240px" /><select class="sel" v-model="bf.shadow" @change="load()"><option value="all">all bans</option><option value="shadow">shadow only</option><option value="hard">hard only</option></select><span class="dim" style="align-self:center">{{ bansTotal }} total</span></div>
      <table><thead><tr><th>Player</th><th style="width:110px">Kind</th><th>Reason</th><th style="width:140px">Until</th><th style="width:120px">By</th><th style="width:140px">Created</th><th style="width:80px"></th></tr></thead><tbody>
        <tr v-for="b in pg1.rows" :key="b.account_id + b.created_at"><td class="row" @click="router.push(`/players/accounts/${b.account_id}?tab=bans`)">{{ b.username ?? short(b.account_id) }}</td><td :class="{ dim: !b.shadow }">{{ b.shadow ? 'shadow' : 'hard' }}{{ (b.features ?? []).length ? ` · ${b.features.join(', ')}` : '' }}</td><td class="dim">{{ b.reason_code ? b.reason_code + ' — ' : '' }}{{ b.reason ?? b.message ?? '' }}</td><td class="dim">{{ b.until ? ts(b.until) : 'permanent' }}</td><td class="dim">{{ b.actor }}</td><td class="dim">{{ ts(b.created_at) }}</td><td><button class="btn s" @click="ask('Lift this ban?') && act(() => post(`/v1/admin/accounts/${b.account_id}/unban`), 'unbanned')">Unban</button></td></tr>
        <tr v-if="!bans.length"><td colspan="7" class="empty">No bans</td></tr></tbody></table><Pager :p="pg1" />
    </template>
    <template v-else>
      <p class="note">Rules act automatically when a player crosses a threshold inside the window: ban, shadow-ban or mute, for the given duration.</p>
      <table><thead><tr><th>Signal</th><th style="width:110px">Threshold</th><th style="width:120px">Window</th><th style="width:110px">Action</th><th style="width:120px">Duration</th><th>Reason</th><th style="width:80px">Enabled</th><th style="width:60px"></th></tr></thead><tbody>
        <tr v-for="r in rules" :key="r.id"><td>{{ r.kind }}</td><td>{{ r.threshold }}</td><td class="dim">{{ Math.round((r.window_secs ?? 0) / 3600) }} h</td><td>{{ r.action }}</td><td class="dim">{{ r.until_secs ? Math.round(r.until_secs / 86400) + ' d' : 'permanent' }}</td><td class="dim">{{ r.reason }}</td><td :class="{ dim: !r.enabled }">{{ r.enabled === false ? 'no' : 'yes' }}</td><td><button class="btn s" @click="ask('Delete rule?') && act(() => api('DELETE', `/v1/admin/bans/rules/${r.id}`), 'rule deleted')">×</button></td></tr>
        <tr v-if="!rules.length"><td colspan="8" class="empty">No automatic rules — every ban is manual</td></tr></tbody></table>
    </template>
  </div>
  <Drawer v-if="dlg === 'decide'" title="Decide report" @close="dlg = ''"><div class="form">
    <JsonView :value="{ target: sel.target_id, category: sel.category, details: sel.details, evidence: sel.evidence }" />
    <div class="f"><label>Decision</label><select class="sel" v-model="form.decision"><option value="warned">warned</option><option value="banned">banned</option><option value="no action">no action</option><option value="muted">muted</option></select></div>
    <div class="f"><label>Note</label><input class="in" v-model="form.note" /></div>
    <div class="f"><label><input type="checkbox" v-model="form.ban" /> also ban the target</label></div>
    <template v-if="form.ban"><div class="f"><label>Ban reason</label><input class="in" v-model="form.reason" /></div><div class="f" style="display:flex;gap:8px"><div><label>Days (0 = permanent)</label><input class="in" type="number" min="0" v-model.number="form.until_days" style="width:120px" /></div><div><label>&nbsp;</label><label><input type="checkbox" v-model="form.shadow" /> shadow ban</label></div></div></template>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="submitDecision">Resolve</button></div></div></Drawer>
  <Drawer v-if="dlg === 'chat'" title="Decide chat report" @close="dlg = ''"><div class="form">
    <div class="f"><label>Action</label><select class="sel" v-model="form.action"><option value="mute">mute in chat</option><option value="kick">kick from channel</option><option value="ban">ban account</option><option value="none">no action</option></select></div>
    <div class="f" v-if="form.action === 'mute' || form.action === 'ban'"><label>Duration (hours)</label><input class="in" type="number" min="0" v-model.number="form.duration_h" /></div>
    <div class="f"><label>Note</label><input class="in" v-model="form.note" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`/v1/admin/chat/reports/${sel.id}`, { status: 'resolved', action: form.action, duration_secs: Number(form.duration_h) * 3600, note: form.note }), 'decided')">Resolve</button></div></div></Drawer>
  <Drawer v-if="dlg === 'rule'" title="Automatic rule" @close="dlg = ''"><div class="form">
    <div class="f"><label>Signal</label><select class="sel" v-model="form.kind"><option value="reports">player reports received</option><option value="chat_reports">chat reports received</option><option value="anticheat">anti-cheat flags</option><option value="fraud">fraud flags</option></select></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Threshold</label><input class="in" type="number" min="1" v-model.number="form.threshold" style="width:110px" /></div><div><label>Window (s)</label><input class="in" type="number" min="60" v-model.number="form.window_secs" style="width:140px" /></div></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Action</label><select class="sel" v-model="form.action"><option value="ban">ban</option><option value="shadow_ban">shadow ban</option><option value="mute">mute</option></select></div><div><label>Duration (s, 0 = permanent)</label><input class="in" type="number" min="0" v-model.number="form.until_secs" style="width:160px" /></div></div>
    <div class="f"><label>Reason shown to the player</label><input class="in" v-model="form.reason" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post('/v1/admin/bans/rules', { ...form, until_secs: form.until_secs || null }), 'rule added')">Add</button></div></div></Drawer>
</template>
