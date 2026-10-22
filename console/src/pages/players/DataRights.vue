<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const router = useRouter(); const ask = (m: string) => window.confirm(m)
const reqs = ref<any[]>([]); const meta = ref<any>({}); const err = ref(''); const filter = ref('all'); const dlg = ref(false); const form = ref({ account_id: '', kind: 'export', include_game_data: true })
async function load() { err.value = ''; try { const r = await get('/v1/server/data_rights'); reqs.value = r.requests ?? []; meta.value = r } catch (x: any) { err.value = x.message } }
onMounted(load)
const view = computed(() => reqs.value.filter((r) => filter.value === 'all' || r.status === filter.value || (filter.value === 'active' && !['done', 'failed', 'cancelled'].includes(r.status))))
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = false; load() } catch (x: any) { toast.err(x) } }
async function download(r: any) { try { const res: Response = await api('GET', `/v1/server/data_rights/${r.id}/download`, undefined, { raw: true }); const blob = await res.blob(); const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = `export-${r.account_id}.zip`; a.click() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => v ? String(v).replace('T', ' ').slice(0, 16) : '—'
const pg1 = usePager(() => view)
</script>
<template>
  <div class="crumb"><span>Data rights</span><span class="id">GDPR / CCPA export &amp; erasure · {{ meta.pending ?? 0 }} pending · worker every {{ meta.poll_minutes ?? '?' }} min</span>
    <span style="margin-left:auto;display:flex;gap:8px"><select class="sel" v-model="filter"><option value="all">all</option><option value="active">active</option><option value="done">done</option><option value="failed">failed</option></select><button class="btn s" @click="act(() => post('/v1/server/data_rights/run'), 'worker triggered')">Run pending now</button><button class="btn s p" @click="dlg = true">New request</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Export bundles every table that references the account into a downloadable archive; erase anonymises the account and deletes personal data while keeping aggregate economics intact.</p>
    <table><thead><tr><th style="width:80px">Kind</th><th>Account</th><th style="width:90px">Status</th><th style="width:140px">Requested</th><th style="width:140px">Completed</th><th style="width:110px">By</th><th>Error</th><th style="width:150px"></th></tr></thead><tbody>
      <tr v-for="r in pg1.rows" :key="r.id"><td>{{ r.kind }}{{ r.include_game_data === false ? ' (no game data)' : '' }}</td><td class="m row" @click="router.push(`/players/accounts/${r.account_id}`)">{{ r.account_id }}</td><td :class="{ bad: r.status === 'failed', ok: r.status === 'running' }">{{ r.status }}</td><td class="dim">{{ ts(r.requested_at) }}</td><td class="dim">{{ ts(r.completed_at) }}{{ r.expires_at ? ` · expires ${ts(r.expires_at)}` : '' }}</td><td class="dim">{{ r.requested_by }}</td><td class="bad">{{ r.error ?? '' }}</td>
        <td><button class="btn s" v-if="r.downloadable" @click="download(r)">Download</button> <button class="btn s" v-if="!['done', 'failed', 'cancelled'].includes(r.status)" @click="ask('Cancel this request?') && act(() => post(`/v1/server/data_rights/${r.id}/cancel`), 'cancelled')">Cancel</button></td></tr>
      <tr v-if="!pg1.total"><td colspan="8" class="empty">No requests</td></tr></tbody></table><Pager :p="pg1" />
  </div>
  <Drawer v-if="dlg" title="New data-rights request" @close="dlg = false"><div class="form">
    <div class="f"><label>Account id</label><input class="in m" v-model="form.account_id" /></div>
    <div class="f"><label>Kind</label><select class="sel" v-model="form.kind"><option value="export">export — produce a downloadable archive</option><option value="erase">erase — anonymise and delete personal data</option></select></div>
    <div class="f"><label><input type="checkbox" v-model="form.include_game_data" /> include game data (inventory, progression, matches)</label></div>
    <div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" :disabled="!form.account_id" @click="(form.kind === 'export' || ask('Erasure is irreversible. Continue?')) && act(() => post('/v1/server/data_rights', form), 'request created')">Create</button></div></div></Drawer>
</template>
