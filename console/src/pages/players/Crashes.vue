<script setup lang="ts">
import { useQueryRef } from '../../composables/query'
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { api, get, qs } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const groups = ref<any[]>([]); const total = ref(0); const err = ref(''); const status = useQueryRef('status', 'open'); const q = useQueryRef('q'); const sel = ref<any>(null); const sample = ref<any>(null); const notes = ref('')
async function load() { err.value = ''; try { const r = await get('/v1/server/crashes' + qs({ status: status.value === 'all' ? undefined : status.value, q: q.value || undefined, limit: 200 })); groups.value = r.groups ?? []; total.value = r.total ?? groups.value.length } catch (x: any) { err.value = x.message } }
onMounted(load)
const view = computed(() => groups.value.filter((g) => !q.value || JSON.stringify(g).toLowerCase().includes(q.value.toLowerCase())))
async function pick(g: any) { sel.value = g; notes.value = g.notes ?? ''; sample.value = g.sample_id ? await get(`/v1/server/crashes/${g.fingerprint}/samples/${g.sample_id}`).catch(() => get(`/v1/server/crashes/${g.fingerprint}`).catch(() => null)) : null }
async function patch(g: any, b: any, ok: string) { try { await api('PATCH', `/v1/server/crashes/${g.fingerprint}`, b); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
async function del(g: any) { try { await api('DELETE', `/v1/server/crashes/${g.fingerprint}`); toast.show('deleted'); sel.value = null; load() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => v ? String(v).replace('T', ' ').slice(0, 16) : '—'
const pg1 = usePager(() => view)
</script>
<template>
  <div class="crumb"><span>Crash reports</span><span class="id">{{ total }} groups · grouped by stack fingerprint</span>
    <span style="margin-left:auto;display:flex;gap:8px"><input class="in" v-model="q" placeholder="filter title / version" style="width:220px" /><select class="sel" v-model="status" @change="load()"><option value="open">open</option><option value="resolved">resolved</option><option value="ignored">ignored</option><option value="all">all</option></select></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div style="display:grid;grid-template-columns:3fr 2fr;gap:12px">
    <div class="card">
      <table><thead><tr><th>Title</th><th style="width:70px">Kind</th><th style="width:70px">Count</th><th style="width:120px">Versions</th><th style="width:130px">Last seen</th><th style="width:90px">Status</th></tr></thead><tbody>
        <tr v-for="g in pg1.rows" :key="g.fingerprint" class="row" :class="{ on: sel?.fingerprint === g.fingerprint }" @click="pick(g)"><td>{{ g.title }}<div class="dim m" style="font-size:11px">{{ g.fingerprint }}{{ g.assignee ? ` · ${g.assignee}` : '' }}</div></td><td class="dim">{{ g.kind }}</td><td>{{ g.count }}</td><td class="dim">{{ g.first_version === g.last_version ? g.last_version : `${g.first_version} → ${g.last_version}` }}</td><td class="dim">{{ ts(g.last_seen) }}</td><td :class="{ dim: g.status !== 'open' }">{{ g.status }}{{ g.resolved_in_version ? ` in ${g.resolved_in_version}` : '' }}</td></tr>
        <tr v-if="!pg1.total"><td colspan="6" class="empty">No crash groups</td></tr></tbody></table><Pager :p="pg1" />
    </div>
    <div class="card" v-if="sel">
      <h3 style="margin-top:0">{{ sel.title }}</h3>
      <Kv k="Fingerprint" :v="sel.fingerprint" mono /><Kv k="Seen" :v="`${sel.count}× · first ${ts(sel.first_seen)} (${sel.first_version}) · last ${ts(sel.last_seen)} (${sel.last_version})`" />
      <Kv k="Assignee"><input class="in" v-model="sel.assignee" @change="patch(sel, { assignee: sel.assignee }, 'assignee saved')" placeholder="who owns this" /></Kv>
      <Kv k="Notes"><textarea class="in" v-model="notes" style="min-height:70px"></textarea><button class="btn s" style="margin-top:6px" @click="patch(sel, { notes }, 'notes saved')">Save notes</button></Kv>
      <div class="row" style="margin:10px 0"><button class="btn s p" v-if="sel.status === 'open'" @click="patch(sel, { status: 'resolved', resolved_in_version: sel.last_version }, 'resolved')">Resolve (fixed in {{ sel.last_version }})</button><button class="btn s" v-if="sel.status === 'open'" @click="patch(sel, { status: 'ignored' }, 'ignored')">Ignore</button><button class="btn s" v-if="sel.status !== 'open'" @click="patch(sel, { status: 'open' }, 'reopened')">Reopen</button><button class="btn s" @click="ask('Delete this crash group and its samples?') && del(sel)">Delete</button></div>
      <h3>Latest sample</h3><JsonView :value="sample ?? { sample_id: sel.sample_id, note: 'sample detail not available' }" />
    </div>
    <div class="card dim" v-else style="display:flex;align-items:center;justify-content:center">Select a group to see the sample, stack and triage controls</div>
  </div>
</template>
