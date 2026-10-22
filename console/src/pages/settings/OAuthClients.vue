<script setup lang="ts">
// Machine clients (partners, tools, game servers) that take tokens from /v1/oauth/token:
// scopes and role, redirect URIs, IP allow-list, secrets (shown once at rotation), live tokens.
import { ref, onMounted } from 'vue'
import { api, get, post, patch } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const dlg = ref<'' | 'new' | 'edit' | 'client'>(''); const form = ref<any>({}); const made = ref<any>(null); const q = ref('')
async function load() { err.value = ''; try { x.value.c = await get('/v1/admin/oauth/clients') } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); await load(); if (x.value.cur) open(x.value.cur.client_id); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
const rows = () => (x.value.c?.clients ?? []).filter((c: any) => !q.value || c.name.includes(q.value) || c.client_id.includes(q.value))
const list = (s: string) => s.split(/[,\n]/).map((v) => v.trim()).filter(Boolean)
function openNew() { dlg.value = 'new'; made.value = null; form.value = { name: '', description: '', kind: 'confidential', role: 'server', scopes: '', redirect_uris: '', allowed_ips: '', token_ttl_secs: 3600, owner: '', secret_label: 'initial' } }
async function create() { const f = form.value; try { made.value = await post('/v1/admin/oauth/clients', { name: f.name, description: f.description, kind: f.kind, role: f.role, scopes: list(f.scopes), redirect_uris: list(f.redirect_uris), allowed_ips: list(f.allowed_ips), token_ttl_secs: Number(f.token_ttl_secs) || 3600, owner: f.owner || null, secret_label: f.secret_label }); toast.show('client created — copy the secret now'); load() } catch (e: any) { toast.err(e) } }
async function open(id: string) { dlg.value = 'client'; try { const [c, t] = await Promise.all([get(`/v1/admin/oauth/clients/${id}`), get(`/v1/admin/oauth/clients/${id}/tokens?all=true`).catch(() => null)]); x.value.cur = c.client ?? c; x.value.tokens = t } catch (e: any) { toast.err(e) } }
function openEdit() { const c = x.value.cur; dlg.value = 'edit'; form.value = { name: c.name, description: c.description ?? '', role: c.role, scopes: (c.scopes ?? []).join(', '), redirect_uris: (c.redirect_uris ?? []).join('\n'), allowed_ips: (c.allowed_ips ?? []).join(', '), token_ttl_secs: c.token_ttl_secs ?? 3600, owner: c.owner ?? '' } }
async function saveEdit() { const f = form.value, id = x.value.cur.client_id; await act(() => patch(`/v1/admin/oauth/clients/${id}`, { name: f.name, description: f.description, role: f.role, scopes: list(f.scopes), redirect_uris: list(f.redirect_uris), allowed_ips: list(f.allowed_ips), token_ttl_secs: Number(f.token_ttl_secs) || undefined, owner: f.owner || null }), 'saved'); dlg.value = 'client' }
async function rotate() { const label = window.prompt('Label for the new secret (the old ones stay valid until deleted)', 'rotated ' + new Date().toISOString().slice(0, 10)); if (label == null) return; const r = await act(() => post(`/v1/admin/oauth/clients/${x.value.cur.client_id}/secrets`, { label }), 'secret issued — copy it now'); if (r) made.value = r }
</script>
<template>
  <div class="crumb"><span>OAuth clients</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s p" @click="openNew">New client</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tools"><input class="in" v-model="q" placeholder="name or client id" /><button class="btn" @click="load">Refresh</button><span class="dim">{{ rows().filter((c: any) => c.enabled).length }} enabled · {{ rows().reduce((a: number, c: any) => a + (c.live_tokens ?? 0), 0) }} live tokens</span></div>
    <DataTable :cols="[{ key: 'name' }, { key: 'client_id', mono: true }, { key: 'kind' }, { key: 'role' }, { key: 'scopes', fmt: (r) => (r.scopes ?? []).join(' ') || '—' }, { key: 'secrets', fmt: (r) => String((r.secrets ?? []).length) }, { key: 'live_tokens', label: 'live tokens' }, { key: 'failures', fmt: (r) => r.failures ? String(r.failures) : '' }, { key: 'last_token_at', label: 'last token', fmt: (r) => ts(r.last_token_at), dim: true }, { key: 'enabled', fmt: (r) => r.enabled ? '' : 'disabled' }]" :rows="rows()" pick @pick="(r) => open(r.client_id)" empty="No OAuth clients — create one for a partner service or tool that needs server-side tokens" />
    <p class="note" style="margin-top:10px">Clients exchange <code>client_id</code> + secret for a token at <code>/v1/oauth/token</code> (client_credentials) and act with the client's role and scopes; <code>failures</code> counts bad-secret attempts.</p>
  </div>
  <Drawer v-if="dlg === 'client' && x.cur" :title="x.cur.name" @close="dlg = ''; x.cur = null">
    <div class="tools"><button class="btn s" @click="openEdit">Edit</button><button class="btn s" @click="rotate">New secret</button><button class="btn s" @click="act(() => patch(`/v1/admin/oauth/clients/${x.cur.client_id}`, { enabled: !x.cur.enabled }), x.cur.enabled ? 'disabled' : 'enabled')">{{ x.cur.enabled ? 'Disable' : 'Enable' }}</button><button class="btn s d" @click="ask('Revoke every live token of this client?') && act(() => api('DELETE', `/v1/admin/oauth/clients/${x.cur.client_id}/tokens`), 'tokens revoked')">Revoke tokens</button><button class="btn s d" @click="ask(`Delete client ${x.cur.name}? Its tokens stop working now.`) && act(async () => { await api('DELETE', `/v1/admin/oauth/clients/${x.cur.client_id}`); dlg = ''; x.cur = null }, 'deleted')">Delete</button></div>
    <div v-if="made" class="card" style="border-color:rgba(245,184,61,.5)"><Kv k="New secret — shown once" :v="made.secret ?? made.client_secret" mono copy /><Kv k="Client id" :v="made.client_id ?? x.cur.client_id" mono copy /></div>
    <Kv k="Client id" :v="x.cur.client_id" mono copy /><Kv k="Kind / role" :v="`${x.cur.kind} / ${x.cur.role}`" /><Kv k="Scopes" :v="(x.cur.scopes ?? []).join(' ') || '—'" /><Kv k="Redirect URIs" :v="(x.cur.redirect_uris ?? []).join(' ') || '—'" mono /><Kv k="Allowed IPs" :v="(x.cur.allowed_ips ?? []).join(' ') || 'any'" mono /><Kv k="Token TTL" :v="x.cur.token_ttl_secs ? x.cur.token_ttl_secs + ' s' : '—'" /><Kv k="Owner" :v="x.cur.owner ?? '—'" /><Kv k="Failures / last token" :v="`${x.cur.failures ?? 0} / ${ts(x.cur.last_token_at)}`" />
    <h3>Secrets <small>({{ (x.cur.secrets ?? []).length }})</small></h3>
    <DataTable :cols="[{ key: 'hint', mono: true }, { key: 'label' }, { key: 'created_at', fmt: (r) => ts(r.created_at), dim: true }, { key: 'expires_at', fmt: (r) => r.expires_at ? ts(r.expires_at) : 'never', dim: true }]" :rows="x.cur.secrets ?? []" empty="No secrets — the client cannot authenticate">
      <template #actions="{ row }"><button class="btn s d" @click="ask('Delete this secret? Anything still using it fails at the next token request.') && act(() => api('DELETE', `/v1/admin/oauth/clients/${x.cur.client_id}/secrets/${row.id}`), 'secret deleted')">Delete</button></template>
    </DataTable>
    <h3>Tokens <small>({{ x.tokens?.total ?? 0 }})</small></h3>
    <DataTable v-if="x.tokens" :cols="[{ key: 'id', mono: true, fmt: (r) => r.id ?? r.jti ?? r.hint ?? '—' }, { key: 'live', fmt: (r) => r.live ?? r.active ? 'live' : r.revoked_at ? 'revoked' : 'expired' }, { key: 'scopes', fmt: (r) => (r.scopes ?? []).join(' ') }, { key: 'created_at', fmt: (r) => ts(r.created_at ?? r.issued_at), dim: true }, { key: 'expires_at', fmt: (r) => ts(r.expires_at), dim: true }, { key: 'ip', mono: true, dim: true }]" :rows="x.tokens.tokens ?? []" empty="No tokens issued" />
    <template v-if="x.cur.metadata && Object.keys(x.cur.metadata).length"><h3>Metadata</h3><JsonView :value="x.cur.metadata" /></template>
  </Drawer>
  <Drawer v-if="dlg === 'new' || dlg === 'edit'" :title="dlg === 'new' ? 'New OAuth client' : 'Edit client'" @close="dlg = dlg === 'edit' ? 'client' : ''"><div class="form">
    <template v-if="made && dlg === 'new'"><div class="card" style="border-color:rgba(245,184,61,.5)"><Kv k="Client id" :v="made.client_id" mono copy /><Kv k="Secret — shown once" :v="made.secret ?? made.client_secret" mono copy /></div></template>
    <div class="f"><label>Name</label><input class="in" v-model="form.name" /></div>
    <div class="f"><label>Description</label><input class="in" v-model="form.description" /></div>
    <div class="f" v-if="dlg === 'new'"><label>Kind</label><select class="in" v-model="form.kind"><option value="confidential">confidential (has a secret)</option><option value="public">public (PKCE, no secret)</option></select></div>
    <div class="f"><label>Role</label><select class="in" v-model="form.role"><option value="server">server</option><option value="admin">admin</option><option value="support">support</option><option value="liveops">liveops</option><option value="ops">ops</option><option value="readonly">readonly</option></select></div>
    <div class="f"><label>Scopes (comma-separated)</label><input class="in" v-model="form.scopes" placeholder="account:read, storage:write" /></div>
    <div class="f"><label>Redirect URIs (one per line)</label><textarea class="in" v-model="form.redirect_uris" style="min-height:60px"></textarea></div>
    <div class="f"><label>Allowed IPs / CIDRs (comma-separated, empty = any)</label><input class="in" v-model="form.allowed_ips" /></div>
    <div class="f"><label>Token TTL (seconds)</label><input class="in" type="number" v-model.number="form.token_ttl_secs" /></div>
    <div class="f"><label>Owner (contact)</label><input class="in" v-model="form.owner" /></div>
    <div class="f" v-if="dlg === 'new'"><label>First secret label</label><input class="in" v-model="form.secret_label" /></div>
    <div class="row"><button class="btn" @click="dlg = dlg === 'edit' ? 'client' : ''">{{ made ? 'Done' : 'Cancel' }}</button><button class="btn p" :disabled="!form.name || !!made" @click="dlg === 'new' ? create() : saveEdit()">{{ dlg === 'new' ? 'Create' : 'Save' }}</button></div>
  </div></Drawer>
</template>
