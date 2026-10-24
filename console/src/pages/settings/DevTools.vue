<script setup lang="ts">
// Developer tooling on a non-production cluster: request recording and replay, fixtures
// (named data sets applied/dropped as a unit), developer flags, auth lock lookup.
import { ref, onMounted } from 'vue'
import { api, get, post, put, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'fixture' | 'recording'>(''); const form = ref<any>({}); const recPath = ref(''); const lockEmail = ref('')
async function load() { err.value = ''; const r = await Promise.allSettled([get('/v1/admin/dev/recording'), get('/v1/admin/dev/recordings' + qs({ path: recPath.value || undefined, limit: 100 })), get('/v1/admin/dev/fixtures'), get('/v1/admin/developers')]); const v = (i: number) => r[i].status === 'fulfilled' ? (r[i] as any).value : (err.value += (r[i] as any).reason?.message + ' ', null); x.value.rec = v(0); x.value.recs = v(1); x.value.fx = v(2); x.value.dev = v(3) }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; load(); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : v).toISOString().replace('T', ' ').slice(0, 19) : '—'
function openFixture() { dlg.value = 'fixture'; form.value = { name: '', reset: false, manifest: JSON.stringify({ accounts: 10 }, null, 2) } }
const demoManifest = () => ({
  accounts: Array.from({ length: 12 }, (_, i) => ({
    username: ['ayla', 'brick', 'cass', 'dorn', 'eiko', 'fenn', 'gale', 'hux', 'iris', 'jude', 'kato', 'lyra'][i],
    display_name: ['Ayla', 'Brick', 'Cass', 'Dorn', 'Eiko', 'Fenn', 'Gale', 'Hux', 'Iris', 'Jude', 'Kato', 'Lyra'][i],
    metadata: { platform: ['steam', 'ios', 'android', 'ps5'][i % 4], country: ['DE', 'US', 'BR', 'JP', 'FR', 'KR'][i % 6], level: 3 + i * 4 },
    wallet: { gold: 250 + i * 175, gems: i % 3 === 0 ? 40 + i * 5 : 0 },
    inventory: [{ item_id: 'sword_basic', quantity: 1 }, { item_id: 'potion_small', quantity: 2 + i }, ...(i % 2 ? [{ item_id: 'shield_wood', quantity: 1 }] : [])],
    storage: [{ collection: 'profile', key: 'settings', value: { music: i % 2 === 0, lang: ['en', 'de', 'pt', 'ja'][i % 4] } }],
  })),
})
// One click of demo data for an empty node: 12 players with wallets, inventory and storage, removable as fixture "demo".
function demo() { act(() => post('/v1/admin/dev/fixtures', { name: 'demo', reset: true, ...demoManifest() }), 'demo fixture applied — 12 players with wallets, inventory and storage') }
async function lock() { const e = lockEmail.value.trim(); if (!e) return; try { x.value.lock = await get(`/v1/admin/auth/locks/${encodeURIComponent(e)}`) } catch (er: any) { x.value.lock = { email: e, error: er.message } } }
</script>
<template>
  <div class="crumb"><span>Developer tools</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <h3 style="margin-top:0">Request recording <small :class="x.rec?.enabled ? 'warn' : ''">{{ x.rec?.enabled ? `on · ${x.rec.sample_percent}% of ${x.rec.path_prefix}` : 'off' }}</small></h3>
    <div class="tools"><button class="btn" :class="x.rec?.enabled ? 'd' : 'p'" @click="act(() => put('/v1/admin/dev/recording', { enabled: !x.rec?.enabled, sample_percent: x.rec?.sample_percent ?? 100, path_prefix: x.rec?.path_prefix ?? '/v1/' }), x.rec?.enabled ? 'recording stopped' : 'recording')">{{ x.rec?.enabled ? 'Stop recording' : 'Start recording' }}</button><button class="btn" @click="dlg = 'recording'; form = { sample_percent: x.rec?.sample_percent ?? 100, path_prefix: x.rec?.path_prefix ?? '/v1/' }">Settings</button><input class="in" v-model="recPath" placeholder="filter by path" @keyup.enter="load" /><button class="btn" @click="load">Refresh</button><button class="btn s d" style="margin-left:auto" @click="ask('Delete every recorded request?') && act(() => api('DELETE', '/v1/admin/dev/recordings'), 'recordings cleared')">Clear all</button></div>
    <DataTable :cols="[{ key: 'at', fmt: (r) => ts(r.at ?? r.created_at), dim: true }, { key: 'method' }, { key: 'path', mono: true }, { key: 'status' }, { key: 'duration_ms', label: 'ms' }, { key: 'actor', fmt: (r) => r.actor ?? r.account_id ?? '', dim: true }]" :rows="x.recs?.recordings ?? []" pick @pick="(r) => (x.pick = r)" empty="No recorded requests — turn recording on and exercise the API">
      <template #actions="{ row }"><button class="btn s" @click="act(async () => { x.replay = await post(`/v1/admin/dev/recordings/${row.id}/replay`); x.pick = null }, 'replayed')">Replay</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">Recording samples live requests (bodies included) for replay against this or another cluster; keep it off in production.</p>
    <template v-if="x.replay"><h3>Last replay</h3><JsonView :value="x.replay" /></template>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Fixtures <small>({{ (x.fx?.fixtures ?? []).length }})</small><button class="btn s p" style="margin-left:10px" @click="openFixture">Apply fixture</button><button class="btn s" style="margin-left:6px" @click="demo">Load demo data</button></h3>
    <DataTable :cols="[{ key: 'name', mono: true }, { key: 'applied_at', fmt: (r) => ts(r.applied_at ?? r.created_at), dim: true }, { key: 'summary', fmt: (r) => r.summary ? JSON.stringify(r.summary) : JSON.stringify(r.manifest ?? r.counts ?? {}), dim: true }]" :rows="x.fx?.fixtures ?? []" empty="No fixtures applied">
      <template #actions="{ row }"><button class="btn s d" @click="ask(`Drop fixture ${row.name} and everything it created?`) && act(() => api('DELETE', `/v1/admin/dev/fixtures/${encodeURIComponent(row.name)}`), 'dropped')">Drop</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">A fixture is a named manifest of test data (accounts, items, worlds…) created in one go and removable in one go — for CI and demo clusters.</p>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Developer accounts <small>({{ (x.dev?.developers ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'account_id', mono: true }, { key: 'note' }, { key: 'set_by', dim: true }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="x.dev?.developers ?? []" empty="No developer-flagged accounts — flag them on the player page to bypass client version gates and see draft content" />
  </div>
  <div class="card">
    <h3 style="margin-top:0">Sign-in locks</h3>
    <div class="tools"><input class="in" v-model="lockEmail" placeholder="email" style="min-width:280px" @keyup.enter="lock" /><button class="btn" :disabled="!lockEmail.trim()" @click="lock">Look up</button><button class="btn d" v-if="x.lock && !x.lock.error" @click="ask(`Clear the lock for ${lockEmail}?`) && act(async () => { await api('DELETE', `/v1/admin/auth/locks/${encodeURIComponent(lockEmail.trim())}`); x.lock = null }, 'lock cleared')">Clear lock</button></div>
    <JsonView v-if="x.lock" :value="x.lock" />
    <p class="note" v-else>Repeated failed password attempts lock an email for a while; look one up to see the counter and clear it.</p>
  </div>
  <Drawer v-if="x.pick" title="Recorded request" @close="x.pick = null"><JsonView :value="x.pick" /></Drawer>
  <Drawer v-if="dlg === 'recording'" title="Recording settings" @close="dlg = ''"><div class="form">
    <div class="f"><label>Sample percent</label><input class="in" type="number" min="1" max="100" v-model.number="form.sample_percent" /></div>
    <div class="f"><label>Path prefix</label><input class="in" v-model="form.path_prefix" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => put('/v1/admin/dev/recording', { enabled: !!x.rec?.enabled, sample_percent: form.sample_percent, path_prefix: form.path_prefix }), 'saved')">Save</button></div>
  </div></Drawer>
  <Drawer v-if="dlg === 'fixture'" title="Apply fixture" @close="dlg = ''"><div class="form">
    <div class="f"><label>Name</label><input class="in" v-model="form.name" placeholder="smoke" /></div>
    <div class="f"><label>Manifest (JSON)</label><textarea class="in" v-model="form.manifest" style="min-height:140px"></textarea></div>
    <div class="f"><label><input type="checkbox" v-model="form.reset" /> reset — drop the fixture first if it exists</label></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.name" @click="act(() => post('/v1/admin/dev/fixtures', { name: form.name, reset: form.reset, ...JSON.parse(form.manifest || '{}') }), 'fixture applied')">Apply</button></div>
  </div></Drawer>
</template>
