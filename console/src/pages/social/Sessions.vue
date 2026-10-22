<script setup lang="ts">
// Game sessions (lobbies/rooms backed by a match or a fleet server) across the cluster: search,
// inspect members, stop one or many; session templates players create their own from.
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const q = ref<any>({ kind: '', name: '' }); const dlg = ref(''); const form = ref<any>({}); const sel = ref<Set<string>>(new Set())
async function load() { err.value = ''; try { const [s, t] = await Promise.all([post('/v1/server/sessions/search', { ...(q.value.kind ? { kind: q.value.kind } : {}), ...(q.value.name ? { name: q.value.name } : {}), limit: 200 }), get('/v1/server/session_templates').catch(() => ({ templates: [] }))]); x.value.s = s; x.value.t = t } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function act(fn: () => Promise<any>, ok: string) { try { const r = await fn(); toast.show(ok); dlg.value = ''; load(); return r } catch (e: any) { toast.err(e) } }
const ts = (v: any) => v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—'
async function open(r: any) { x.value.cur = r; x.value.members = await get(`/v1/server/sessions/${r.id}/members`).catch((e) => ({ error: e.message })) }
function toggle(id: string) { const s = new Set(sel.value); s.has(id) ? s.delete(id) : s.add(id); sel.value = s }
async function stopSel(dry: boolean) { const r = await act(() => post('/v1/server/sessions/stop', { ids: [...sel.value], reason: 'console', dry_run: dry }), dry ? 'dry run done' : 'stopped'); if (r) x.value.stop = r; if (!dry) sel.value = new Set() }
function openTemplate(t?: any) { dlg.value = 'tpl'; form.value = { id: t?.id ?? '', text: JSON.stringify(t ? { ...t, id: undefined } : { name: '', kind: 'custom', max_players: 4, join_policy: 'open', properties: {}, data: {} }, null, 2) } }
</script>
<template>
  <div class="crumb"><span>Game sessions</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" :disabled="!sel.size" @click="stopSel(true)">Dry-run stop ({{ sel.size }})</button><button class="btn s d" :disabled="!sel.size" @click="ask(`Stop ${sel.size} sessions?`) && stopSel(false)">Stop selected</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="tools"><input class="in" v-model="q.name" placeholder="name" @keyup.enter="load" /><input class="in" v-model="q.kind" placeholder="kind" style="max-width:140px" @keyup.enter="load" /><button class="btn" @click="load">Search</button><span class="dim">{{ (x.s?.sessions ?? []).length }} sessions · {{ (x.s?.sessions ?? []).reduce((a: number, s: any) => a + (s.max_players - s.free_slots), 0) }} players seated</span></div>
    <table><thead><tr><th style="width:30px"></th><th>Name</th><th>Kind</th><th>Players</th><th>Join</th><th>Backing</th><th>Owner</th><th>Created</th></tr></thead><tbody>
      <tr v-for="s in x.s?.sessions ?? []" :key="s.id" class="row" @click="open(s)"><td @click.stop><input type="checkbox" :checked="sel.has(s.id)" @change="toggle(s.id)" /></td><td>{{ s.name }}<div class="dim m" style="font-size:11px">{{ s.id }}</div></td><td>{{ s.kind }}</td><td>{{ s.max_players - s.free_slots }} / {{ s.max_players }}</td><td>{{ s.join_policy }}</td><td class="dim">{{ s.match_id ? 'match …' + s.match_id.slice(-8) : s.fleet_id ? 'fleet …' + s.fleet_id.slice(-8) : 'none' }}</td><td class="m dim">…{{ (s.owner_id ?? '').slice(-8) }}</td><td class="dim">{{ ts(s.created_at) }}</td></tr>
      <tr v-if="!(x.s?.sessions ?? []).length"><td colspan="8" class="empty">No sessions match</td></tr></tbody></table>
    <JsonView v-if="x.stop" :value="x.stop" />
  </div>
  <div class="card">
    <h3 style="margin-top:0">Session templates <small>({{ (x.t?.templates ?? []).length }})</small><button class="btn s p" style="margin-left:10px" @click="openTemplate()">Add template</button></h3>
    <DataTable :cols="[{ key: 'id', mono: true }, { key: 'name' }, { key: 'kind' }, { key: 'max_players' }, { key: 'join_policy' }]" :rows="x.t?.templates ?? []" empty="No templates — players can only join sessions the server creates">
      <template #actions="{ row }"><button class="btn s" @click="openTemplate(row)">Edit</button><button class="btn s d" @click="ask(`Delete template ${row.id}?`) && act(() => api('DELETE', `/v1/server/session_templates/${encodeURIComponent(row.id)}`), 'deleted')">Delete</button></template>
    </DataTable>
    <p class="note" style="margin-top:10px">A template is what a player's "create room" button offers: kind, size, join policy and default properties; the server fills in fleet or match.</p>
  </div>
  <Drawer v-if="x.cur" :title="x.cur.name" @close="x.cur = null">
    <div class="tools"><button class="btn s d" @click="ask('Stop this session?') && act(async () => { await post('/v1/server/sessions/stop', { ids: [x.cur.id], reason: 'console' }); x.cur = null }, 'stopped')">Stop</button></div>
    <h3 style="margin-top:0">Members</h3>
    <DataTable v-if="x.members && !x.members.error" :cols="[{ key: 'account_id', mono: true }, { key: 'role', fmt: (r) => r.role ?? (r.account_id === x.cur.owner_id ? 'owner' : 'member') }, { key: 'joined_at', fmt: (r) => ts(r.joined_at), dim: true }]" :rows="x.members.members ?? []" empty="Empty session">
      <template #actions="{ row }"><button class="btn s d" @click="ask('Remove this member?') && act(async () => { await api('DELETE', `/v1/server/sessions/${x.cur.id}/members/${row.account_id}`); open(x.cur) }, 'removed')">Remove</button></template>
    </DataTable>
    <h3>Session</h3><JsonView :value="x.cur" />
  </Drawer>
  <Drawer v-if="dlg === 'tpl'" :title="form.id ? form.id : 'New template'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Id</label><input class="in" v-model="form.id" placeholder="duel" /></div>
    <div class="f"><label>Template (JSON)</label><textarea class="in" v-model="form.text" style="min-height:180px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" :disabled="!form.id" @click="act(() => put(`/v1/server/session_templates/${encodeURIComponent(form.id)}`, JSON.parse(form.text)), 'saved')">Save</button></div>
  </div></Drawer>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
