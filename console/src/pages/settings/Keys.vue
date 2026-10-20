<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const rows = ref<any[]>([]); const err = ref(''); const show = ref<'active' | 'revoked' | 'all'>('active'); const q = ref(''); const dlg = ref(''); const form = ref<any>({}); const created = ref<any>(null); const sel = ref<any>(null)
const roles = [['server', 'server — game servers and backend jobs, full /v1/server'], ['admin', 'admin — everything incl. operators and keys'], ['ops', 'ops — fleets, cluster, builds'], ['liveops', 'liveops — flags, experiments, config, economy'], ['support', 'support — players, bans, grants']]
async function load() { err.value = ''; try { rows.value = (await get('/v1/admin/keys')).keys ?? [] } catch (e: any) { err.value = e.message } }
onMounted(load)
const view = computed(() => rows.value.filter((k) => (show.value === 'all' ? true : show.value === 'revoked' ? k.revoked_at : !k.revoked_at && !(k.expires_at && k.expires_at * 1000 < Date.now()))).filter((k) => !q.value || JSON.stringify(k).toLowerCase().includes(q.value.toLowerCase())))
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
const state = (k: any) => k.revoked_at ? 'revoked' : k.expires_at && k.expires_at * 1000 < Date.now() ? 'expired' : 'active'
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
async function create() { try { created.value = await post('/v1/admin/keys', { name: form.value.name, role: form.value.role, expires_in_days: form.value.expires_in_days || null }); toast.show('key created'); load() } catch (e: any) { toast.err(e) } }
function copy(s: string) { navigator.clipboard?.writeText(s); toast.show('copied') }
</script>
<template>
  <div class="crumb"><span>Operator keys</span><span class="id">{{ rows.filter((k) => state(k) === 'active').length }} active · {{ rows.length }} total</span>
    <span style="margin-left:auto;display:flex;gap:8px;align-items:center"><input class="in" v-model="q" placeholder="filter name / subject / id" style="width:220px" /><select class="sel" v-model="show"><option value="active">active</option><option value="revoked">revoked</option><option value="all">all</option></select><button class="btn s p" @click="dlg = 'new'; created = null; form = { name: '', role: 'support', expires_in_days: 90 }">Create key</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Keys authenticate operators and servers on <code>x-musubi-server-key</code>. The secret is shown once at creation; revoking takes effect at once on every node. Limits throttle a key's requests per second.</p>
    <DataTable :cols="[{ key: 'name' }, { key: 'role' }, { key: 'subject', fmt: (r) => r.subject ?? '', dim: true }, { key: 'state', fmt: state }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'last_used_at', fmt: (r) => ts(r.last_used_at), dim: true }, { key: 'expires_at', fmt: (r) => ts(r.expires_at), dim: true }, { key: 'limit', fmt: (r) => r.limit ? `${r.limit.rps} rps / burst ${r.limit.burst}` : '—', dim: true }]" :rows="view">
      <template #actions="{ row }"><template v-if="!row.revoked_at"><button class="btn s" @click="sel = row; dlg = 'limits'; form = { rps: row.limit?.rps ?? null, burst: row.limit?.burst ?? null }">Limits</button> <button class="btn s" @click="ask(`Revoke key “${row.name}”? Clients using it stop working at once.`) && act(() => api('DELETE', `/v1/admin/keys/${row.id}`), 'revoked')">Revoke</button></template></template>
    </DataTable>
  </div>
  <Drawer v-if="dlg === 'new'" title="Create operator key" @close="dlg = ''"><div class="form">
    <template v-if="!created">
      <div class="f"><label>Name</label><input class="in" v-model="form.name" placeholder="ci-deploy, anna@studio" /></div>
      <div class="f"><label>Role</label><select class="sel" v-model="form.role"><option v-for="[v, l] in roles" :key="v" :value="v">{{ l }}</option></select></div>
      <div class="f"><label>Expires in days (empty = never)</label><input class="in" type="number" min="1" v-model.number="form.expires_in_days" /></div>
      <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.name" @click="create">Create</button></div>
    </template>
    <template v-else>
      <p class="note">Copy the secret now — it is not stored and cannot be shown again.</p>
      <div class="f"><label>Key id</label><code class="m">{{ created.id }}</code></div>
      <div class="f"><label>Secret</label><div style="display:flex;gap:8px"><input class="in m" :value="created.secret" readonly style="flex:1" /><button class="btn s" @click="copy(created.secret)">Copy</button></div></div>
      <div class="row"><button class="btn p" @click="dlg = ''">Done</button></div>
    </template>
  </div></Drawer>
  <Drawer v-if="dlg === 'limits'" :title="`Rate limit — ${sel?.name}`" @close="dlg = ''"><div class="form">
    <div class="f"><label>Requests per second (empty = unlimited)</label><input class="in" type="number" min="1" v-model.number="form.rps" /></div>
    <div class="f"><label>Burst</label><input class="in" type="number" min="1" v-model.number="form.burst" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => put(`/v1/admin/keys/${sel.id}/limits`, { rps: form.rps || null, burst: form.burst || null }), 'limits saved')">Save</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
