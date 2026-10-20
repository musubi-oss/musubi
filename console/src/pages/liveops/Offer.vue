<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new'); const O = computed(() => `/v1/server/offers/${encodeURIComponent(id.value)}`)
const o = ref<any>(null); const err = ref(''); const stats = ref<any>(null); const placements = ref<any[]>([]); const storeItems = ref<any[]>([]); const audiences = ref<string[]>([]); const currencies = ref<string[]>([]); const offers = ref<any[]>([])
interface P { currency: string; amount: number }
const prices = ref<P[]>([]); const starts = ref(''); const ends = ref(''); const meta = ref('{}'); const dlg = ref(false); const acc = ref('')
const toIso = (v: any) => v ? new Date(v * 1000).toISOString().slice(0, 16) : ''
const fromIso = (x: string) => x ? Math.floor(new Date(x).getTime() / 1000) : null
async function load() {
  err.value = ''
  try {
    if (isNew.value) o.value = { name: '', description: '', placement_id: '', store_item_id: '', priority: 0, enabled: true, audience: null, max_purchases: null, max_activations: null, cooldown_secs: 0, duration_secs: null, precursor: null, metadata: {} }
    else { const r = await get(O.value); o.value = r.offer ?? r; stats.value = await get(`${O.value}/stats`).catch(() => null) }
    prices.value = Object.entries(o.value.prices ?? {}).map(([c, a]) => ({ currency: c, amount: Number(a) })); starts.value = toIso(o.value.starts_at); ends.value = toIso(o.value.ends_at); meta.value = JSON.stringify(o.value.metadata ?? {}, null, 2)
  } catch (x: any) { err.value = x.message }
  const [p, s, a, c, os] = await Promise.allSettled([get('/v1/server/placements'), get('/v1/server/store'), get('/v1/server/audiences'), get('/v1/server/currencies'), get('/v1/server/offers')])
  placements.value = p.status === 'fulfilled' ? p.value.placements ?? [] : []; storeItems.value = s.status === 'fulfilled' ? s.value.items ?? [] : []; audiences.value = a.status === 'fulfilled' ? (a.value.audiences ?? []).map((x: any) => x.name) : []; currencies.value = c.status === 'fulfilled' ? (c.value.currencies ?? []).map((x: any) => x.code) : []; offers.value = os.status === 'fulfilled' ? (os.value.offers ?? []).filter((x: any) => x.id !== id.value) : []
}
onMounted(load)
const item = computed(() => storeItems.value.find((s) => s.id === o.value?.store_item_id))
function body() { return { ...o.value, prices: prices.value.length ? Object.fromEntries(prices.value.filter((p) => p.currency).map((p) => [p.currency, Number(p.amount)])) : null, starts_at: fromIso(starts.value), ends_at: fromIso(ends.value), metadata: JSON.parse(meta.value || '{}'), audience: o.value.audience || null, precursor: o.value.precursor || null, max_purchases: o.value.max_purchases || null, max_activations: o.value.max_activations || null, duration_secs: o.value.duration_secs || null, cooldown_secs: Number(o.value.cooldown_secs || 0), priority: Number(o.value.priority || 0) } }
async function save() { try { const b = body(); const r = isNew.value ? await post('/v1/server/offers', b) : await put(O.value, b); toast.show('saved'); if (isNew.value) router.replace(`/liveops/offers/${encodeURIComponent(r.id ?? r.offer?.id)}`); else load() } catch (x: any) { toast.err(x) } }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = false; load() } catch (x: any) { toast.err(x) } }
const pct = (a: any, b: any) => b ? `${((Number(a) / Number(b)) * 100).toFixed(1)}%` : '—'
const fmt = (s: number) => !s ? '—' : s >= 86400 ? `${(s / 86400).toFixed(1)}d` : s >= 3600 ? `${(s / 3600).toFixed(1)}h` : `${Math.round(s / 60)}m`
</script>
<template>
  <div class="crumb"><router-link to="/liveops/offers">Offers</router-link> › <span>{{ isNew ? 'new offer' : o?.name || id }}</span><span class="id" v-if="!isNew">{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="!isNew" @click="dlg = true; acc = ''">Force for player</button><button class="btn s" v-if="!isNew" @click="ask('Delete this offer?') && act(async () => { await api('DELETE', O); router.push('/liveops/offers') }, 'deleted')">Delete</button><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="o">
    <div class="grid">
      <div><Kv k="Name"><input class="in" v-model="o.name" /></Kv><Kv k="Description"><input class="in" v-model="o.description" /></Kv><Kv k="Enabled"><label><input type="checkbox" v-model="o.enabled" /> eligible players can see it</label></Kv><Kv k="Priority (higher wins a placement slot)"><input class="in" type="number" v-model.number="o.priority" /></Kv></div>
      <div><Kv k="Placement"><input class="in m" v-model="o.placement_id" list="placements" placeholder="main_menu, shop_banner…" /></Kv><Kv k="Store item delivered"><input class="in m" v-model="o.store_item_id" list="store" /></Kv><Kv k="Item preview" :v="item ? `${item.item_id} × ${item.quantity} · base ${Object.entries(item.prices ?? {}).map(([c, a]) => `${a} ${c}`).join(' / ') || 'free'}` : '—'" /><Kv k="Audience"><select class="sel" v-model="o.audience"><option :value="null">everyone</option><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option></select></Kv></div>
      <div><Kv k="Window"><input class="in" type="datetime-local" v-model="starts" /><input class="in" type="datetime-local" v-model="ends" style="margin-top:6px" /></Kv><Kv k="Per-player limits: purchases / activations"><div style="display:flex;gap:8px"><input class="in" type="number" min="1" v-model.number="o.max_purchases" placeholder="∞" /><input class="in" type="number" min="1" v-model.number="o.max_activations" placeholder="∞" /></div></Kv><Kv k="Cooldown between activations (s) / visible for (s)"><div style="display:flex;gap:8px"><input class="in" type="number" min="0" v-model.number="o.cooldown_secs" /><input class="in" type="number" min="0" v-model.number="o.duration_secs" placeholder="until window ends" /></div></Kv><Kv k="Precursor (shown only after this offer was bought)"><select class="sel" v-model="o.precursor"><option :value="null">none</option><option v-for="x in offers" :key="x.id" :value="x.id">{{ x.name }}</option></select></Kv></div>
    </div>
    <h3>Offer price <small>({{ prices.length ? 'overrides the store item price' : 'empty = store item price' }})</small> <button class="btn s p" style="margin-left:8px" @click="prices.push({ currency: '', amount: 1 })">Add price</button></h3>
    <table><thead><tr><th style="width:260px">Currency</th><th style="width:160px">Amount</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(p, i) in prices" :key="i"><td><input class="in m" style="min-width:0;width:100%" v-model="p.currency" list="currencies" /></td><td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="p.amount" /></td><td><button class="btn s" @click="prices.splice(i, 1)">×</button></td></tr>
      <tr v-if="!prices.length"><td colspan="3" class="empty">Store item price applies</td></tr></tbody></table>
    <template v-if="stats"><h3>Funnel</h3>
      <div class="grid"><div><Kv k="Seen / accounts" :v="`${stats.seen ?? 0} / ${stats.accounts ?? 0}`" /><Kv k="Activations · active now" :v="`${stats.activations ?? 0} · ${stats.active ?? 0}`" /></div><div><Kv k="Purchased" :v="`${stats.purchased ?? 0} (${pct(stats.purchased, stats.seen)} of seen)`" /><Kv k="Dismissed · expired" :v="`${stats.dismissed ?? 0} · ${stats.expired ?? 0}`" /></div><div><Kv k="Conversion" :v="stats.conversion != null ? `${(Number(stats.conversion) * 100).toFixed(1)}%` : '—'" /><Kv k="Revenue" :v="(stats.revenue ?? []).length ? stats.revenue.map((r: any) => `${r.amount} ${r.currency}`).join(' / ') : (stats.amount != null ? `${stats.amount} ${stats.currency ?? ''}` : '—')" /></div></div></template>
    <h3>Metadata (JSON)</h3><textarea class="in m" v-model="meta" style="min-height:80px"></textarea>
    <datalist id="placements"><option v-for="p in placements" :key="p.id ?? p" :value="p.id ?? p" /></datalist><datalist id="store"><option v-for="s in storeItems" :key="s.id" :value="s.id">{{ s.item_id }}</option></datalist><datalist id="currencies"><option v-for="c in currencies" :key="c" :value="c" /></datalist>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="o" />
    <p class="dim" v-if="!isNew" style="margin-top:8px">Cooldown {{ fmt(o.cooldown_secs) }} · visible {{ fmt(o.duration_secs) }}</p>
  </div>
  <Drawer v-if="dlg" title="Force this offer for a player" @close="dlg = false"><div class="form"><p class="note">Bypasses audience, window and precursor for one account (QA / support). Remove with the same account id.</p><div class="f"><label>Account id</label><input class="in m" v-model="acc" /></div><div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn s" :disabled="!acc" @click="act(() => api('DELETE', `${O}/force/${acc}`), 'force removed')">Remove force</button><button class="btn p" :disabled="!acc" @click="act(() => put(`${O}/force/${acc}`, { account_id: acc }), 'forced')">Force</button></div></div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
