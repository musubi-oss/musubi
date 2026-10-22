<script setup lang="ts">
// Redeem-code campaigns: a campaign names the reward (reward table or platform key group), its
// limits and audience; codes are generated in batches or added as shared codes; redemptions
// are listed per campaign; a single code can be revoked.
import { ref, computed, onMounted } from 'vue'
import { get, post, put, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'campaign' | 'generate' | 'shared'>(''); const form = ref<any>({}); const q = ref(''); const cur = ref<any>(null); const unused = ref(''); const batch = ref('')
async function load() { err.value = ''; try { x.value.c = await get('/v1/server/codes/campaigns') } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; await load(); if (cur.value) openCampaign(cur.value.id); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
const rows = computed(() => (x.value.c?.campaigns ?? []).filter((c: any) => !q.value || c.id.includes(q.value) || c.name.includes(q.value)))
async function openCampaign(id: string) { const c = (x.value.c?.campaigns ?? []).find((r: any) => r.id === id); cur.value = c; const [codes, red] = await Promise.all([get(`/v1/server/codes/campaigns/${encodeURIComponent(id)}/codes` + qs({ unused: unused.value || undefined, batch: batch.value || undefined, limit: 200 })).catch((e) => ({ error: e.message })), get(`/v1/server/codes/campaigns/${encodeURIComponent(id)}/redemptions?limit=200`).catch((e) => ({ error: e.message }))]); x.value.codes = codes; x.value.red = red }
function openForm(c?: any) { dlg.value = 'campaign'; form.value = c ? { ...c, metadata: JSON.stringify(c.metadata ?? {}), starts_at: c.starts_at ? new Date(c.starts_at * 1000).toISOString().slice(0, 16) : '', ends_at: c.ends_at ? new Date(c.ends_at * 1000).toISOString().slice(0, 16) : '' } : { id: '', name: '', reward_table: '', key_group: '', per_account: 1, max_redemptions: null, starts_at: '', ends_at: '', audience: '', enabled: true, metadata: '{}' } }
const epoch = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)
async function save() { const f = form.value; let metadata: any; try { metadata = JSON.parse(f.metadata || '{}') } catch { toast.err(new Error('metadata must be JSON')); return } await act(() => put(`/v1/server/codes/campaigns/${encodeURIComponent(f.id)}`, { name: f.name, reward_table: f.reward_table || null, key_group: f.key_group || null, per_account: Number(f.per_account) || 1, max_redemptions: f.max_redemptions === null || f.max_redemptions === '' ? null : Number(f.max_redemptions), starts_at: epoch(f.starts_at), ends_at: epoch(f.ends_at), audience: f.audience || null, enabled: !!f.enabled, metadata }), 'saved') }
async function generate() { const f = form.value; const r = await act(() => post(`/v1/server/codes/campaigns/${encodeURIComponent(cur.value.id)}/generate`, { count: Number(f.count), length: Number(f.length), prefix: f.prefix || undefined, max_uses: Number(f.max_uses) || 1, batch: f.batch || undefined }), 'codes generated'); if (r) x.value.generated = r }
const codesList = computed<any[]>(() => x.value.codes?.codes ?? [])
const redList = computed<any[]>(() => x.value.red?.redemptions ?? [])
</script>
<template>
  <div class="crumb"><span>Code campaigns</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="openForm()">New campaign</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tools"><input class="in" v-model="q" placeholder="id or name" /><button class="btn" @click="load">Refresh</button><span class="dim">{{ rows.filter((c: any) => c.enabled).length }} enabled</span></div>
    <DataTable :cols="[{ key: 'id', mono: true }, { key: 'name' }, { key: 'reward', fmt: (r) => r.reward_table ? `table ${r.reward_table}` : r.key_group ? `keys ${r.key_group}` : '—' }, { key: 'per_account', label: 'per account' }, { key: 'max_redemptions', label: 'max', fmt: (r) => r.max_redemptions ?? '∞' }, { key: 'window', fmt: (r) => r.starts_at || r.ends_at ? `${ts(r.starts_at)} → ${ts(r.ends_at)}` : 'always' }, { key: 'audience', fmt: (r) => r.audience ?? 'everyone' }, { key: 'enabled', fmt: (r) => r.enabled ? '' : 'disabled' }]" :rows="rows" pick @pick="(r) => openCampaign(r.id)" empty="No campaigns — create one, then generate codes or add a shared code" />
    <p class="note" style="margin-top:10px">A player redeems at <code>POST /v1/codes/redeem</code>; the campaign's reward table is granted, or one key from its key group is handed out. Per-account and total caps, window and audience gate every redemption.</p>
  </div>
  <Drawer v-if="cur" :title="cur.name" @close="cur = null">
    <div class="tools"><button class="btn s" @click="openForm(cur)">Edit</button><button class="btn s p" @click="dlg = 'generate'; form = { count: 100, length: 10, prefix: '', max_uses: 1, batch: new Date().toISOString().slice(0, 10) }; x.generated = null">Generate codes</button><button class="btn s" @click="dlg = 'shared'; form = { code: '', max_uses: 1000 }">Add shared code</button><button class="btn s" @click="act(() => put(`/v1/server/codes/campaigns/${encodeURIComponent(cur.id)}`, { ...cur, enabled: !cur.enabled }), cur.enabled ? 'disabled' : 'enabled')">{{ cur.enabled ? 'Disable' : 'Enable' }}</button></div>
    <Kv k="Id" :v="cur.id" mono copy /><Kv k="Reward" :v="cur.reward_table ? `reward table ${cur.reward_table}` : cur.key_group ? `key group ${cur.key_group}` : 'none'" /><Kv k="Limits" :v="`${cur.per_account} per account · ${cur.max_redemptions ?? 'unlimited'} total`" /><Kv k="Window" :v="cur.starts_at || cur.ends_at ? `${ts(cur.starts_at)} → ${ts(cur.ends_at)}` : 'always'" /><Kv k="Audience" :v="cur.audience ?? 'everyone'" />
    <h3>Codes <small>({{ codesList.length }}{{ x.codes?.total != null ? ' of ' + x.codes.total : '' }})</small></h3>
    <div class="tools"><select class="in" v-model="unused" @change="openCampaign(cur.id)"><option value="">all</option><option value="true">unused</option><option value="false">used</option></select><input class="in" v-model="batch" placeholder="batch" @keyup.enter="openCampaign(cur.id)" /><button class="btn" @click="openCampaign(cur.id)">Filter</button></div>
    <DataTable v-if="!x.codes?.error" :cols="[{ key: 'code', mono: true }, { key: 'uses', fmt: (r) => `${r.uses ?? r.used ?? 0} / ${r.max_uses ?? 1}` }, { key: 'batch', dim: true }, { key: 'revoked_at', label: 'revoked', fmt: (r) => r.revoked_at ? ts(r.revoked_at) : '', dim: true }]" :rows="codesList" empty="No codes yet">
      <template #actions="{ row }"><button class="btn s d" v-if="!row.revoked_at" @click="ask(`Revoke code ${row.code}?`) && act(() => post(`/v1/server/codes/${encodeURIComponent(row.code)}/revoke`), 'revoked')">Revoke</button></template>
    </DataTable>
    <p class="err" v-else>{{ x.codes.error }}</p>
    <h3>Redemptions <small>({{ redList.length }})</small></h3>
    <DataTable v-if="!x.red?.error" :cols="[{ key: 'at', fmt: (r) => ts(r.at ?? r.redeemed_at ?? r.created_at), dim: true }, { key: 'code', mono: true }, { key: 'account_id', mono: true }, { key: 'result', fmt: (r) => r.result ?? r.status ?? r.key ?? '', dim: true }]" :rows="redList" empty="Nobody has redeemed yet" />
    <p class="err" v-else>{{ x.red.error }}</p>
  </Drawer>
  <Drawer v-if="dlg === 'campaign'" :title="form.created_at ? form.name : 'New campaign'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Id</label><input class="in" v-model="form.id" placeholder="launch" :disabled="!!form.created_at" /></div>
    <div class="f"><label>Name</label><input class="in" v-model="form.name" /></div>
    <div class="f"><label>Reward table id (or empty)</label><input class="in" v-model="form.reward_table" /></div>
    <div class="f"><label>Key group id (platform keys, or empty)</label><input class="in" v-model="form.key_group" /></div>
    <div class="f"><label>Per account / max redemptions (empty = unlimited)</label><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="form.per_account" /><input class="in" type="number" v-model="form.max_redemptions" /></div></div>
    <div class="f"><label>Window (local time, empty = always)</label><div style="display:flex;gap:8px"><input class="in" type="datetime-local" v-model="form.starts_at" /><input class="in" type="datetime-local" v-model="form.ends_at" /></div></div>
    <div class="f"><label>Audience (name, empty = everyone)</label><input class="in" v-model="form.audience" /></div>
    <div class="f"><label>Metadata (JSON)</label><textarea class="in" v-model="form.metadata" style="min-height:60px"></textarea></div>
    <div class="f"><label><input type="checkbox" v-model="form.enabled" /> enabled</label></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.id || !form.name" @click="save">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'generate'" title="Generate codes" @close="dlg = ''"><div class="form">
    <div class="f"><label>Count / length</label><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="form.count" /><input class="in" type="number" v-model.number="form.length" /></div></div>
    <div class="f"><label>Prefix</label><input class="in" v-model="form.prefix" placeholder="LAUNCH-" /></div>
    <div class="f"><label>Max uses per code</label><input class="in" type="number" v-model.number="form.max_uses" /></div>
    <div class="f"><label>Batch label</label><input class="in" v-model="form.batch" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Close</button><button class="btn p" :disabled="!form.count" @click="generate">Generate</button></div>
    <template v-if="x.generated"><h3>Generated <small>(copy now — they are also in the list)</small></h3><JsonView :value="x.generated" /></template>
  </div></Drawer>
  <Drawer v-if="dlg === 'shared'" title="Add shared code" @close="dlg = ''"><div class="form">
    <p class="note">One human-readable code many players redeem (streamer drops, newsletters).</p>
    <div class="f"><label>Code</label><input class="in" v-model="form.code" placeholder="WELCOME2026" /></div>
    <div class="f"><label>Max uses</label><input class="in" type="number" v-model.number="form.max_uses" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.code" @click="act(() => post(`/v1/server/codes/campaigns/${encodeURIComponent(cur.id)}/codes`, { code: form.code, max_uses: form.max_uses }), 'code added')">Add</button></div>
  </div></Drawer>
</template>
