<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const provs = ref<any[]>([]); const open = ref(0); const err = ref(''); const dlg = ref(false); const p = ref<any>(null); const isNew = ref(false); const scopes = ref(''); const clientIds = ref(''); const claims = ref('{}'); const secret = ref(''); const test = ref<any>(null)
async function load() { err.value = ''; try { const r = await get('/v1/admin/auth/providers'); provs.value = r.providers ?? []; open.value = r.open_flows ?? 0 } catch (x: any) { err.value = x.message } }
onMounted(load)
function edit(x: any) {
  isNew.value = !x; p.value = JSON.parse(JSON.stringify(x ?? { name: '', kind: 'oidc', display_name: '', enabled: true, allow_signup: true, issuer: '', authorize_url: '', token_url: '', userinfo_url: '', jwks_url: '', client_id: '', auth_style: 'body', claims: { subject: 'sub', email: 'email', email_verified: 'email_verified', username: 'preferred_username', display_name: 'name', avatar_url: 'picture' } }))
  scopes.value = (p.value.scopes ?? []).join(' '); clientIds.value = (p.value.client_ids ?? []).join(', '); claims.value = JSON.stringify(p.value.claims ?? {}, null, 2); secret.value = ''; test.value = null; dlg.value = true
}
async function save() {
  try {
    const { name, has_client_secret: _h, ...rest } = p.value
    const b: any = { ...rest, scopes: scopes.value.split(/\s+/).filter(Boolean), client_ids: clientIds.value.split(',').map((s) => s.trim()).filter(Boolean), claims: JSON.parse(claims.value || '{}') }
    if (secret.value) b.client_secret = secret.value
    await put(`/v1/admin/auth/providers/${encodeURIComponent(name)}`, b); toast.show('provider saved'); dlg.value = false; load()
  } catch (x: any) { toast.err(x) }
}
async function runTest(name: string) { try { test.value = await post(`/v1/admin/auth/providers/${encodeURIComponent(name)}/test`); toast.show(test.value?.ok === false ? 'test failed' : 'test ok') } catch (x: any) { toast.err(x) } }
async function del(name: string) { try { await api('DELETE', `/v1/admin/auth/providers/${encodeURIComponent(name)}`); toast.show('deleted'); load() } catch (x: any) { toast.err(x) } }
const kinds = [['oidc', 'OpenID Connect (discovery by issuer)'], ['oauth2', 'OAuth 2 (explicit endpoints)'], ['steam', 'Steam'], ['epic', 'Epic'], ['xbox', 'Xbox Live'], ['psn', 'PlayStation Network'], ['nintendo', 'Nintendo'], ['apple', 'Apple'], ['google', 'Google'], ['discord', 'Discord'], ['facebook', 'Facebook'], ['gamecenter', 'Game Center']]
const pg1 = usePager(() => provs)
</script>
<template>
  <div class="crumb"><span>Identity providers</span><span class="id">{{ provs.filter((x) => x.enabled).length }} enabled of {{ provs.length }} · {{ open }} open login flows</span><span style="margin-left:auto"><button class="btn s p" @click="edit(null)">Add provider</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Each provider is a login button in the client: clients call <code>/v1/auth/{name}</code>. Secrets are stored sealed and never shown again.</p>
    <table><thead><tr><th>Name</th><th style="width:110px">Kind</th><th>Issuer / authorize URL</th><th style="width:90px">Signup</th><th style="width:90px">Secret</th><th style="width:80px">Enabled</th><th style="width:170px"></th></tr></thead><tbody>
      <tr v-for="x in pg1.rows" :key="x.name"><td class="m row" @click="edit(x)">{{ x.name }}<span class="dim" v-if="x.display_name"> — {{ x.display_name }}</span></td><td>{{ x.kind }}</td><td class="m dim">{{ x.issuer || x.authorize_url || '—' }}</td><td class="dim">{{ x.allow_signup ? 'allowed' : 'link only' }}</td><td class="dim">{{ x.has_client_secret ? 'set' : '—' }}</td><td :class="x.enabled ? 'ok' : 'dim'">{{ x.enabled ? 'yes' : 'no' }}</td><td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" @click="runTest(x.name)">Test</button> <button class="btn s" @click="ask(`Delete provider ${x.name}? Players linked through it cannot log in with it any more.`) && del(x.name)">×</button></td></tr>
      <tr v-if="!pg1.total"><td colspan="7" class="empty">No providers — device / email login only</td></tr></tbody></table><Pager :p="pg1" />
    <pre class="pre" v-if="test && !dlg" style="margin-top:10px;max-height:200px">{{ JSON.stringify(test, null, 2) }}</pre>
  </div>
  <Drawer v-if="dlg" :title="isNew ? 'New identity provider' : `Provider ${p.name}`" @close="dlg = false"><div class="form">
    <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Name (URL slug)</label><input class="in m" v-model="p.name" :disabled="!isNew" placeholder="steam, google, studio_sso" /></div><div style="flex:1"><label>Display name</label><input class="in" v-model="p.display_name" /></div></div>
    <div class="f"><label>Kind</label><select class="sel" v-model="p.kind"><option v-for="[k, l] in kinds" :key="k" :value="k">{{ l }}</option></select></div>
    <div class="f"><label><input type="checkbox" v-model="p.enabled" /> enabled</label> &nbsp; <label><input type="checkbox" v-model="p.allow_signup" /> allow new accounts (else link to existing only)</label></div>
    <template v-if="p.kind === 'oidc' || p.kind === 'oauth2'">
      <div class="f"><label>Issuer (OIDC discovery)</label><input class="in m" v-model="p.issuer" placeholder="https://accounts.example.com" /></div>
      <div class="f" v-if="p.kind === 'oauth2'"><label>Authorize / token / userinfo / JWKS URLs</label><input class="in m" v-model="p.authorize_url" placeholder="authorize" /><input class="in m" v-model="p.token_url" placeholder="token" style="margin-top:6px" /><input class="in m" v-model="p.userinfo_url" placeholder="userinfo" style="margin-top:6px" /><input class="in m" v-model="p.jwks_url" placeholder="jwks (optional)" style="margin-top:6px" /></div>
      <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Client id</label><input class="in m" v-model="p.client_id" /></div><div style="flex:1"><label>Client secret {{ p.has_client_secret ? '(set — leave empty to keep)' : '' }}</label><input class="in m" type="password" v-model="secret" autocomplete="new-password" /></div></div>
      <div class="f" style="display:flex;gap:8px"><div style="flex:1"><label>Scopes (space-separated)</label><input class="in m" v-model="scopes" placeholder="openid email profile" /></div><div><label>Client auth</label><select class="sel" v-model="p.auth_style"><option value="body">body</option><option value="basic">basic header</option></select></div></div>
      <div class="f"><label>Claim mapping (JSON: our field → their claim)</label><textarea class="in m" v-model="claims" style="min-height:120px"></textarea></div>
    </template>
    <template v-else>
      <div class="f"><label>Accepted client / app ids (comma-separated)</label><input class="in m" v-model="clientIds" /></div>
      <div class="f"><label>Platform secret / API key {{ p.has_client_secret ? '(set — leave empty to keep)' : '' }}</label><input class="in m" type="password" v-model="secret" autocomplete="new-password" /></div>
    </template>
    <pre class="pre" v-if="test" style="max-height:160px">{{ JSON.stringify(test, null, 2) }}</pre>
    <div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn s" v-if="!isNew" @click="runTest(p.name)">Test</button><button class="btn p" :disabled="!p.name" @click="save">Save</button></div></div></Drawer>
</template>
