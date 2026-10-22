<script setup lang="ts">
// Hands-on economy operations for support and LiveOps: grant an item, repair a durable, adjust
// a wallet (with reason and idempotency key so a retry never pays twice), send in-game mail to
// one account or a whole audience (count first), and the per-account grant limits that cap
// what any single operator action can hand out.
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref('')
const grant = ref<any>({ account_id: '', item_id: '', quantity: 1, instance: '' })
const repair = ref<any>({ account_id: '', id: '', amount: 10 })
const adjust = ref<any>({ account_id: '', currency: '', delta: 0, reason: '', reference: '', idempotency_key: '' })
const mail = ref<any>({ account_id: '', audience: '', title: '', body: '', rewards: '', expires_secs: 0 })
const limit = ref<any>({ kind: 'item', key: '', window_secs: 3600, max_per_account: 1 })
async function load() { err.value = ''; try { x.value.limits = await get('/v1/server/grant_limits') } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function run(k: string, fn: () => Promise<any>, ok: string) { try { x.value[k] = await fn(); toast.show(ok); load() } catch (e: any) { toast.err(e) } }
const parseJson = (s: string, fallback: any) => { if (!s.trim()) return fallback; return JSON.parse(s) }
const newKey = () => (adjust.value.idempotency_key = 'console-' + Date.now().toString(36))
function mailBody() { const m = mail.value; return { account_ids: m.account_id ? [m.account_id] : [], all: false, audience: m.audience || null, title: m.title, body: m.body, rewards: m.rewards || null, metadata: {}, expires_in_seconds: Number(m.expires_secs) || null } }
</script>
<template>
  <div class="crumb"><span>Grants &amp; wallet</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px">
    <div class="card"><h3 style="margin-top:0">Grant item</h3>
      <div class="form"><div class="f"><label>Account id</label><input class="in" v-model="grant.account_id" /></div><div class="f"><label>Item id</label><input class="in" v-model="grant.item_id" /></div><div class="f"><label>Quantity</label><input class="in" type="number" v-model.number="grant.quantity" /></div><div class="f"><label>Instance data (JSON, for unique items)</label><input class="in" v-model="grant.instance" placeholder='{"rarity":"epic"}' /></div>
        <div class="row"><button class="btn p" :disabled="!grant.account_id || !grant.item_id" @click="run('grant', () => post('/v1/server/inventory/grant', { account_id: grant.account_id, item_id: grant.item_id, quantity: grant.quantity, instance: grant.instance ? parseJson(grant.instance, undefined) : undefined }), 'granted')">Grant</button></div></div>
      <JsonView v-if="x.grant" :value="x.grant" />
    </div>
    <div class="card"><h3 style="margin-top:0">Adjust wallet</h3>
      <div class="form"><div class="f"><label>Account id</label><input class="in" v-model="adjust.account_id" /></div><div class="f"><label>Currency / delta (negative to deduct)</label><div style="display:flex;gap:8px"><input class="in" v-model="adjust.currency" placeholder="gold" /><input class="in" type="number" v-model.number="adjust.delta" /></div></div><div class="f"><label>Reason (shown in the ledger)</label><input class="in" v-model="adjust.reason" placeholder="support: refund for ticket 123" /></div><div class="f"><label>Reference / idempotency key</label><div style="display:flex;gap:8px"><input class="in" v-model="adjust.reference" placeholder="ticket-123" /><input class="in" v-model="adjust.idempotency_key" placeholder="unique per adjustment" /><button class="btn s" @click="newKey">new</button></div></div>
        <div class="row"><button class="btn" :class="adjust.delta < 0 ? 'd' : 'p'" :disabled="!adjust.account_id || !adjust.currency || !adjust.delta || !adjust.reason" @click="ask(`${adjust.delta > 0 ? 'Credit' : 'Debit'} ${Math.abs(adjust.delta)} ${adjust.currency}?`) && run('adjust', () => post('/v1/server/wallet/adjust', { ...adjust, idempotency_key: adjust.idempotency_key || undefined, reference: adjust.reference || undefined }), 'wallet adjusted')">Apply</button></div></div>
      <JsonView v-if="x.adjust" :value="x.adjust" />
    </div>
    <div class="card"><h3 style="margin-top:0">Repair durable item</h3>
      <div class="form"><div class="f"><label>Account id</label><input class="in" v-model="repair.account_id" /></div><div class="f"><label>Inventory entry id</label><input class="in" v-model="repair.id" /></div><div class="f"><label>Durability to restore</label><input class="in" type="number" v-model.number="repair.amount" /></div>
        <div class="row"><button class="btn p" :disabled="!repair.account_id || !repair.id" @click="run('repair', () => post('/v1/server/inventory/repair', repair), 'repaired')">Repair</button></div></div>
      <JsonView v-if="x.repair" :value="x.repair" />
    </div>
    <div class="card"><h3 style="margin-top:0">Send mail</h3>
      <div class="form"><div class="f"><label>Account id — or audience name for a bulk send</label><div style="display:flex;gap:8px"><input class="in" v-model="mail.account_id" placeholder="one account" /><input class="in" v-model="mail.audience" placeholder="audience" /></div></div><div class="f"><label>Title</label><input class="in" v-model="mail.title" /></div><div class="f"><label>Body</label><textarea class="in" v-model="mail.body" style="min-height:70px"></textarea></div><div class="f"><label>Reward table id (optional) / expires in seconds</label><div style="display:flex;gap:8px"><input class="in" v-model="mail.rewards" placeholder="welcome-pack" /><input class="in" type="number" v-model.number="mail.expires_secs" style="max-width:140px" /></div></div>
        <div class="row"><button class="btn" :disabled="!mail.audience" @click="run('count', () => post('/v1/server/mail/audience/count', { audience: mail.audience }), 'counted')">Count recipients</button><button class="btn p" :disabled="!(mail.account_id || mail.audience) || !mail.title" @click="(!mail.audience || ask(`Send to the whole audience ${mail.audience}${x.count?.count != null ? ' (' + x.count.count + ' accounts)' : ''}?`)) && run('mail', () => post('/v1/server/mail', mailBody()), 'sent')">Send</button></div></div>
      <JsonView v-if="x.count || x.mail" :value="x.mail ?? x.count" />
    </div>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Grant limits <small>({{ (x.limits?.limits ?? []).length }})</small></h3>
    <p class="note">Caps how much of one item or currency any account can receive from server grants inside a window — a brake on a leaked server key or a runaway script, not on players.</p>
    <div class="tools"><select class="in" v-model="limit.kind"><option value="item">item</option><option value="currency">currency</option></select><input class="in" v-model="limit.key" placeholder="item / currency id" /><input class="in" type="number" v-model.number="limit.max_per_account" style="max-width:120px" placeholder="max" /><input class="in" type="number" v-model.number="limit.window_secs" style="max-width:140px" placeholder="window s" /><button class="btn p" :disabled="!limit.key" @click="run('lim', () => put('/v1/server/grant_limits', limit), 'limit saved')">Set limit</button></div>
    <DataTable :cols="[{ key: 'kind' }, { key: 'key', mono: true }, { key: 'max_per_account', label: 'max per account' }, { key: 'window_secs', label: 'window', fmt: (r) => r.window_secs >= 3600 ? (r.window_secs / 3600) + ' h' : r.window_secs + ' s' }]" :rows="x.limits?.limits ?? []" empty="No grant limits — every server grant goes through unchecked">
      <template #actions="{ row }"><button class="btn s d" @click="ask('Delete this limit?') && run('lim', () => api('DELETE', `/v1/server/grant_limits/${row.kind}/${encodeURIComponent(row.key)}`), 'deleted')">Delete</button></template>
    </DataTable>
  </div>
</template>
