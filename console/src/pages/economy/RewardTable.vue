<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new')
const t = ref<any>(null); const err = ref(''); const items = ref<string[]>([]); const currencies = ref<string[]>([]); const dlg = ref<'' | 'grant'>(''); const form = ref<any>({})
interface E { kind: 'item' | 'currency' | 'table'; ref: string; amount: number; rarity: string; weight: number }
const entries = ref<E[]>([]); const deferred = ref('')
async function load() {
  err.value = ''
  try {
    if (isNew.value) t.value = { id: '', name: '', kind: 'weighted', enabled: true, pity: { after: 0, rarity: '' } }
    else { const r = await get('/v1/server/rewards/tables'); t.value = (r.tables ?? []).find((x: any) => x.id === id.value); if (!t.value) throw new Error('table not found') }
    t.value.pity ||= { after: 0, rarity: '' }
    entries.value = (t.value.entries ?? []).map((e: any) => ({ kind: e.item_id ? 'item' : e.currency ? 'currency' : 'table', ref: e.item_id ?? e.currency ?? e.table_id ?? '', amount: e.amount ?? 1, rarity: e.rarity ?? 'common', weight: e.weight ?? 1 }))
    deferred.value = t.value.deferred ? JSON.stringify(t.value.deferred) : ''
  } catch (e: any) { err.value = e.message }
  try { const [i, c] = await Promise.all([get('/v1/server/items'), get('/v1/server/currencies')]); items.value = (i.items ?? []).map((x: any) => x.id); currencies.value = (c.currencies ?? []).map((x: any) => x.code) } catch {}
}
onMounted(load)
const total = computed(() => entries.value.reduce((s, e) => s + Number(e.weight || 0), 0))
function body() {
  return { id: t.value.id || undefined, name: t.value.name, kind: t.value.kind, enabled: t.value.enabled, pity: t.value.pity?.after ? t.value.pity : null, deferred: deferred.value.trim() ? JSON.parse(deferred.value) : null,
    entries: entries.value.map((e) => ({ item_id: e.kind === 'item' ? e.ref : null, currency: e.kind === 'currency' ? e.ref : null, table_id: e.kind === 'table' ? e.ref : undefined, amount: Number(e.amount), rarity: e.rarity, weight: Number(e.weight) })) }
}
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; return r } catch (e: any) { toast.err(e) } }
async function save() {
  const b = body()
  const r = await act(() => post('/v1/server/rewards/tables', b), 'saved')
  if (r && isNew.value) router.replace(`/economy/reward_tables/${encodeURIComponent(r.id ?? r.table?.id ?? b.id)}`); else load()
}
</script>
<template>
  <div class="crumb"><router-link to="/economy/reward_tables">Reward tables</router-link> › <span>{{ isNew ? 'new table' : t?.name || id }}</span><span class="id" v-if="!isNew">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="t">
      <button class="btn s" v-if="!isNew" @click="dlg = 'grant'; form = { account_id: '', reason: 'console' }">Grant to player</button>
      <button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="t">
    <div class="grid">
      <div><Kv k="Id"><input class="in m" v-model="t.id" :disabled="!isNew" placeholder="auto" /></Kv><Kv k="Name"><input class="in" v-model="t.name" /></Kv></div>
      <div><Kv k="Kind"><select class="sel" v-model="t.kind"><option value="weighted">weighted — one roll by weight</option><option value="all">all — every entry</option><option value="sequence">sequence — next entry per grant</option></select></Kv><Kv k="Enabled"><label><input type="checkbox" v-model="t.enabled" /> grants pay out</label></Kv></div>
      <div><Kv k="Pity — guarantee rarity after N misses"><div style="display:flex;gap:8px"><input class="in" type="number" min="0" v-model.number="t.pity.after" style="max-width:100px" placeholder="0 = off" /><input class="in" v-model="t.pity.rarity" placeholder="rarity (e.g. legendary)" /></div></Kv><Kv k="Deferred (JSON, optional: claim later)"><input class="in m" v-model="deferred" placeholder='{"ttl_secs": 86400}' /></Kv></div>
    </div>
    <h3>Entries <small>({{ entries.length }}, weights sum {{ total }})</small> <button class="btn s p" style="margin-left:8px" @click="entries.push({ kind: 'item', ref: '', amount: 1, rarity: 'common', weight: 1 })">Add entry</button></h3>
    <table><thead><tr><th style="width:120px">Type</th><th>Item / currency / table</th><th style="width:110px">Amount</th><th style="width:140px">Rarity</th><th style="width:110px">Weight</th><th style="width:160px">Chance</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(e, i) in entries" :key="i">
        <td><select class="sel" style="min-width:0;width:100%" v-model="e.kind"><option value="item">item</option><option value="currency">currency</option><option value="table">table</option></select></td>
        <td><input class="in m" style="min-width:0;width:100%" v-model="e.ref" :list="e.kind === 'item' ? 'items' : e.kind === 'currency' ? 'currencies' : undefined" /></td>
        <td><input class="in" style="min-width:0;width:100%" type="number" min="1" v-model.number="e.amount" /></td>
        <td><input class="in" style="min-width:0;width:100%" v-model="e.rarity" list="rarities" /></td>
        <td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="e.weight" /></td>
        <td><template v-if="t.kind === 'weighted'"><div class="bar"><i :style="{ width: (total ? (e.weight / total) * 100 : 0) + '%' }"></i></div><span class="dim">{{ total ? ((e.weight / total) * 100).toFixed(1) : 0 }}%</span></template><span class="dim" v-else>always</span></td>
        <td><button class="btn s d" @click="entries.splice(i, 1)">×</button></td>
      </tr>
      <tr v-if="!entries.length"><td colspan="7" class="empty">No entries — grants pay nothing</td></tr></tbody></table>
    <datalist id="items"><option v-for="i in items" :key="i" :value="i" /></datalist>
    <datalist id="currencies"><option v-for="c in currencies" :key="c" :value="c" /></datalist>
    <datalist id="rarities"><option value="common" /><option value="uncommon" /><option value="rare" /><option value="epic" /><option value="legendary" /></datalist>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="t" />
  </div>
  <Drawer v-if="dlg === 'grant'" title="Grant this table to a player" @close="dlg = ''"><div class="form">
    <div class="f"><label>Account id</label><input class="in m" v-model="form.account_id" /></div><div class="f"><label>Reason (ledger)</label><input class="in" v-model="form.reason" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.account_id" @click="act(() => post('/v1/server/rewards/grant', { account_id: form.account_id, table_id: id, reason: form.reason }), 'granted')">Grant</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
