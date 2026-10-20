<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new')
const s = ref<any>(null); const err = ref(''); const items = ref<string[]>([]); const currencies = ref<string[]>([]); const audiences = ref<string[]>([])
interface P { currency: string; amount: number }
const prices = ref<P[]>([]); const bundle = ref(''); const aud = ref<string[]>([]); const plat = ref(''); const from = ref(''); const until = ref(''); const discUntil = ref('')
function toIso(v: any) { return v ? new Date(typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : v).toISOString().slice(0, 16) : '' }
function fromIso(x: string) { return x ? Math.floor(new Date(x).getTime() / 1000) : null }
async function load() {
  err.value = ''
  try {
    if (isNew.value) s.value = { id: '', item_id: '', quantity: 1, enabled: true, discount_percent: 0, max_per_account: null, min_level: null, level_track: null, rotation: null, rotation_slots: 0 }
    else { const r = await get('/v1/server/store'); s.value = (r.items ?? []).find((x: any) => x.id === id.value); if (!s.value) throw new Error('listing not found') }
    prices.value = Object.entries(s.value.prices ?? {}).map(([c, a]) => ({ currency: c, amount: Number(a) })); bundle.value = JSON.stringify(s.value.bundle ?? []); aud.value = [...(s.value.audiences ?? [])]; plat.value = (s.value.platforms ?? []).join(', ')
    from.value = toIso(s.value.available_from); until.value = toIso(s.value.available_until); discUntil.value = toIso(s.value.discount_until)
  } catch (e: any) { err.value = e.message }
  try { const [i, c, a] = await Promise.all([get('/v1/server/items'), get('/v1/server/currencies'), get('/v1/server/audiences')]); items.value = (i.items ?? []).map((x: any) => x.id); currencies.value = (c.currencies ?? []).map((x: any) => x.code); audiences.value = (a.audiences ?? []).map((x: any) => x.name) } catch {}
}
onMounted(load)
async function save() {
  try {
    const b = { ...s.value, id: s.value.id || undefined, prices: Object.fromEntries(prices.value.filter((p) => p.currency).map((p) => [p.currency, Number(p.amount)])), bundle: JSON.parse(bundle.value || '[]'), audiences: aud.value, platforms: plat.value.split(',').map((x) => x.trim()).filter(Boolean), available_from: fromIso(from.value), available_until: fromIso(until.value), discount_until: fromIso(discUntil.value), quantity: Number(s.value.quantity), discount_percent: Number(s.value.discount_percent || 0), max_per_account: s.value.max_per_account || null, min_level: s.value.min_level || null, level_track: s.value.level_track || null, rotation: s.value.rotation || null, rotation_slots: Number(s.value.rotation_slots || 0) }
    const r = await post('/v1/server/store', b); toast.show('listing saved')
    if (isNew.value) router.replace(`/economy/store/${encodeURIComponent(r.id ?? r.item?.id ?? b.id)}`); else load()
  } catch (e: any) { toast.err(e) }
}
</script>
<template>
  <div class="crumb"><router-link to="/economy/store">Store</router-link> › <span class="m">{{ isNew ? 'new listing' : id }}</span><span style="margin-left:auto"><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="s">
    <div class="grid">
      <div><Kv k="Listing id"><input class="in m" v-model="s.id" :disabled="!isNew" placeholder="auto" /></Kv><Kv k="Item"><input class="in m" v-model="s.item_id" list="items" /></Kv><Kv k="Quantity per purchase"><input class="in" type="number" min="1" v-model.number="s.quantity" /></Kv><Kv k="Enabled"><label><input type="checkbox" v-model="s.enabled" /> visible and purchasable</label></Kv></div>
      <div><Kv k="Available from / until"><input class="in" type="datetime-local" v-model="from" /><input class="in" type="datetime-local" v-model="until" style="margin-top:6px" /></Kv><Kv k="Discount % / until"><div style="display:flex;gap:8px"><input class="in" type="number" min="0" max="100" v-model.number="s.discount_percent" style="max-width:100px" /><input class="in" type="datetime-local" v-model="discUntil" /></div></Kv><Kv k="Max per account"><input class="in" type="number" min="1" v-model.number="s.max_per_account" placeholder="unlimited" /></Kv></div>
      <div><Kv k="Audiences"><select class="sel" multiple v-model="aud" style="min-height:90px"><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option></select></Kv><Kv k="Platforms (comma-separated, empty = all)"><input class="in" v-model="plat" placeholder="ios, android" /></Kv><Kv k="Min level / track"><div style="display:flex;gap:8px"><input class="in" type="number" v-model.number="s.min_level" placeholder="any" style="max-width:100px" /><input class="in m" v-model="s.level_track" placeholder="track id" /></div></Kv><Kv k="Rotation / slots"><div style="display:flex;gap:8px"><input class="in" v-model="s.rotation" placeholder="rotation name" /><input class="in" type="number" min="0" v-model.number="s.rotation_slots" style="max-width:100px" /></div></Kv></div>
    </div>
    <h3>Prices <small>({{ prices.length }} currencies — any one pays)</small> <button class="btn s p" style="margin-left:8px" @click="prices.push({ currency: '', amount: 1 })">Add price</button></h3>
    <table><thead><tr><th style="width:260px">Currency</th><th style="width:160px">Amount</th><th style="width:50px"></th></tr></thead><tbody>
      <tr v-for="(p, i) in prices" :key="i"><td><input class="in m" style="min-width:0;width:100%" v-model="p.currency" list="currencies" /></td><td><input class="in" style="min-width:0;width:100%" type="number" min="0" v-model.number="p.amount" /></td><td><button class="btn s d" @click="prices.splice(i, 1)">×</button></td></tr>
      <tr v-if="!prices.length"><td colspan="3" class="empty">Free</td></tr></tbody></table>
    <h3>Bundle contents <small>(JSON: [{item_id, quantity}] — extra items delivered with the purchase)</small></h3>
    <textarea class="in m" v-model="bundle" style="min-height:80px"></textarea>
    <datalist id="items"><option v-for="i in items" :key="i" :value="i" /></datalist><datalist id="currencies"><option v-for="c in currencies" :key="c" :value="c" /></datalist>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="s" />
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
