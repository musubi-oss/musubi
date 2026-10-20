<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const ops = ref<any[]>([]); const invites = ref<any[]>([]); const sso = ref<any>(null); const jwt = ref<any>(null); const err = ref(''); const dlg = ref(''); const form = ref<any>({}); const made = ref<any>(null)
async function load() {
  err.value = ''
  const r = await Promise.allSettled([get('/v1/admin/operators'), get('/v1/admin/operators/invites'), get('/v1/admin/sso'), get('/v1/admin/jwt/keys')])
  const v = (i: number) => (r[i].status === 'fulfilled' ? (r[i] as any).value : (err.value += (r[i] as any).reason?.message + ' ', null))
  ops.value = v(0)?.members ?? []; invites.value = v(1)?.invites ?? []; sso.value = v(2); jwt.value = v(3)
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (e: any) { toast.err(e) } }
async function invite() { try { made.value = await post('/v1/admin/operators/invites', { email: form.value.email, role: form.value.role, expires_in_hours: form.value.hours || undefined }); toast.show('invite created'); load() } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? String(typeof v === 'number' ? new Date(v * 1000).toISOString() : v).replace('T', ' ').slice(0, 16) : '—'
const inviteState = (i: any) => i.decided_at ? (i.accepted ? 'accepted' : 'declined') : i.expired ? 'expired' : 'pending'
</script>
<template>
  <div class="crumb"><span>Operators &amp; access</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="dlg = 'invite'; made = null; form = { email: '', role: 'support', hours: 72 }">Invite operator</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="cards2" style="display:grid;grid-template-columns:1fr 1fr;gap:12px">
    <div class="card"><h3>SSO <small>(OIDC)</small></h3>
      <template v-if="sso"><Kv k="Status"><span :class="sso.enabled ? 'ok' : 'dim'">{{ sso.enabled ? 'enabled' : 'disabled — operators sign in with keys' }}</span></Kv><Kv k="Issuer" :v="sso.issuer ?? '—'" mono /><Kv k="Login URL" :v="sso.login_url ?? '—'" mono /><Kv k="Default role" :v="sso.default_role ?? '—'" /><Kv k="Session" :v="`${sso.session_hours} h`" /><Kv k="Group → role mappings" :v="(sso.roles ?? []).length ? JSON.stringify(sso.roles) : 'none'" /></template>
      <p class="note">Configured in <code>[sso]</code> on the node; this card reflects what the cluster currently serves.</p></div>
    <div class="card"><h3>Player JWT signing keys <small>({{ jwt?.keys?.length ?? 0 }})</small> <button class="btn s" style="margin-left:8px" @click="ask('Rotate: a new key becomes active, the old one keeps verifying until retired.') && act(() => post('/v1/admin/jwt/rotate'), 'rotated')">Rotate</button></h3>
      <template v-if="jwt"><Kv k="Active kid" :v="jwt.active" mono />
        <table><thead><tr><th>kid</th><th>Created</th><th>Retired</th><th></th></tr></thead><tbody>
          <tr v-for="k in jwt.keys" :key="k.kid"><td class="m" :class="{ ok: k.kid === jwt.active }">{{ k.kid }}{{ k.kid === jwt.active ? ' · active' : '' }}</td><td class="dim">{{ ts(k.created_at) }}</td><td class="dim">{{ ts(k.retired_at) }}</td><td><button class="btn s d" v-if="k.kid !== jwt.active && !k.retired_at" @click="ask(`Retire ${k.kid}? Tokens signed with it stop verifying.`) && act(() => api('DELETE', `/v1/admin/jwt/keys/${k.kid}`), 'retired')">Retire</button></td></tr></tbody></table></template></div>
  </div>
  <div class="card" style="margin-top:12px"><h3>Operators <small>({{ ops.length }})</small></h3>
    <DataTable :cols="[{ key: 'email', fmt: (r) => r.email ?? r.subject ?? r.id }, { key: 'name', fmt: (r) => r.name ?? '' }, { key: 'role' }, { key: 'source', fmt: (r) => r.source ?? r.provider ?? '', dim: true }, { key: 'last_login_at', fmt: (r) => ts(r.last_login_at ?? r.last_seen_at), dim: true }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }]" :rows="ops">
      <template #actions="{ row }"><button class="btn s d" @click="ask(`Remove operator ${row.email ?? row.id}?`) && act(() => api('DELETE', `/v1/admin/operators/${row.id}`), 'removed')">Remove</button></template>
    </DataTable>
    <p class="empty" v-if="!ops.length">No SSO / invited operators yet — access is by keys only</p></div>
  <div class="card" style="margin-top:12px"><h3>Invites <small>({{ invites.length }})</small></h3>
    <DataTable :cols="[{ key: 'email' }, { key: 'role' }, { key: 'state', fmt: inviteState }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'expires_at', fmt: (r) => ts(r.expires_at), dim: true }]" :rows="invites">
      <template #actions="{ row }"><button class="btn s d" v-if="inviteState(row) === 'pending'" @click="act(() => api('DELETE', `/v1/admin/operators/invites/${row.id}`), 'invite revoked')">Revoke</button></template>
    </DataTable></div>
  <Drawer v-if="dlg === 'invite'" title="Invite operator" @close="dlg = ''"><div class="form">
    <template v-if="!made"><div class="f"><label>Email</label><input class="in" v-model="form.email" /></div><div class="f"><label>Role</label><select class="sel" v-model="form.role"><option>admin</option><option>ops</option><option>liveops</option><option>support</option></select></div><div class="f"><label>Valid for hours</label><input class="in" type="number" min="1" v-model.number="form.hours" /></div>
      <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.email" @click="invite">Create invite</button></div></template>
    <template v-else><p class="note">Send this to the operator; it is shown once.</p><pre class="pre">{{ JSON.stringify(made, null, 2) }}</pre><div class="row"><button class="btn p" @click="dlg = ''">Done</button></div></template>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
