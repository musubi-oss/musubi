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
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new'); const C = computed(() => `/v1/server/economy/changesets/${encodeURIComponent(id.value)}`)
const c = ref<any>(null); const err = ref(''); const diff = ref<any>(null); const validation = ref<any>(null); const preview = ref<any>(null); const dlg = ref(''); const form = ref<any>({}); const sched = ref(''); const tab = ref<'changes' | 'diff' | 'preview'>('changes')
const tables = ['items', 'currencies', 'store_items', 'reward_tables', 'progression_tracks', 'seasons', 'quests', 'achievements', 'economy_strings', 'energies', 'entitlements', 'iap_products', 'leaderboards', 'rating_queues', 'streaks', 'content_tags', 'crafting_recipes', 'store_coupons']
const toIso = (v: any) => v ? new Date(v * 1000).toISOString().slice(0, 16) : ''
async function load() {
  err.value = ''
  try {
    if (isNew.value) { c.value = { name: '', note: '', schedule_at: null, changes: [] }; return }
    const r = await get(C.value); c.value = r.changeset ?? r; sched.value = toIso(c.value.schedule_at)
    const [d, v] = await Promise.allSettled([get(`${C.value}/diff`), get(`${C.value}/validate`)]); diff.value = d.status === 'fulfilled' ? d.value : null; validation.value = v.status === 'fulfilled' ? v.value : { ok: false, problems: [{ problem: (v as any).reason?.message }] }
  } catch (x: any) { err.value = x.message }
}
onMounted(load)
const changes = computed<any[]>(() => {
  const raw = c.value?.changes ?? c.value?.staged ?? []
  if (Array.isArray(raw)) return raw
  return Object.entries(raw).flatMap(([table, rows]: [string, any]) => (Array.isArray(rows) ? rows.map((r: any): any[] => [r.key, r]) : Object.entries(rows ?? {})).map((e: any[]) => { const [k, r] = e; return ({ table, op: r?.op ?? (r === null || r?.deleted || r?.delete ? 'delete' : 'upsert'), key: k, row: r?.row ?? r ?? {} }) }))
})
const editable = computed(() => isNew.value || c.value?.state === 'draft' || c.value?.state === 'rejected')
const flow: Record<string, string> = { draft: 'draft → submit for review', submitted: 'awaiting review → approve / reject', approved: 'approved → publish (or waits for schedule)', scheduled: 'scheduled → publishes automatically', published: 'published', rejected: 'rejected → fix and resubmit', cancelled: 'cancelled' }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); dlg.value = ''; load() } catch (x: any) { toast.err(x) } }
async function create() { try { const r = await post('/v1/server/economy/changesets', { name: c.value.name, note: c.value.note, schedule_at: sched.value ? Math.floor(new Date(sched.value).getTime() / 1000) : null }); toast.show('created'); router.replace(`/liveops/changesets/${encodeURIComponent(r.id ?? r.changeset?.id)}`) } catch (x: any) { toast.err(x) } }
async function stage() { await act(() => put(`${C.value}/changes`, { changes: [{ table: form.value.table, op: form.value.op, row: JSON.parse(form.value.row || '{}') }] }), 'change staged') }
const keyOf = (ch: any) => ch.key ?? ch.row?.id ?? ch.row?.code ?? ch.row?.name ?? ch.row?.key ?? '?'
async function loadPreview() { tab.value = 'preview'; if (!preview.value) preview.value = await get(`${C.value}/preview`).catch((x) => ({ error: x.message })) }
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
</script>
<template>
  <div class="crumb"><router-link to="/liveops/changesets">Scheduled changesets</router-link> › <span>{{ isNew ? 'new changeset' : c?.name || id }}</span><span class="id" v-if="!isNew">{{ c?.state }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="c">
      <template v-if="isNew"><button class="btn s p" :disabled="!c.name" @click="create">Create draft</button></template>
      <template v-else>
        <button class="btn s" v-if="editable" @click="dlg = 'stage'; form = { table: 'items', op: 'upsert', row: '{\n  \n}' }">Stage change</button>
        <button class="btn s p" v-if="['draft', 'rejected'].includes(c.state)" :disabled="!changes.length" @click="act(() => post(`${C}/submit`), 'submitted for review')">Submit</button>
        <button class="btn s p" v-if="c.state === 'submitted'" @click="dlg = 'approve'; form = { note: '' }">Approve</button>
        <button class="btn s" v-if="c.state === 'submitted'" @click="dlg = 'reject'; form = { note: '' }">Reject</button>
        <button class="btn s p" v-if="c.state === 'approved'" @click="ask('Publish to the live economy now? A content version is created and all players see it.') && act(() => post(`${C}/publish`), 'published')">Publish now</button>
        <button class="btn s" v-if="!['published', 'cancelled'].includes(c.state)" @click="ask('Cancel this changeset?') && act(() => post(`${C}/cancel`), 'cancelled')">Cancel</button>
        <button class="btn s" v-if="['draft', 'cancelled', 'rejected'].includes(c.state)" @click="ask('Delete permanently?') && act(async () => { await api('DELETE', C); router.push('/liveops/changesets') }, 'deleted')">Delete</button>
      </template></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="c">
    <div class="grid">
      <div><Kv k="Name"><input class="in" v-model="c.name" :disabled="!isNew" /></Kv><Kv k="Note"><input class="in" v-model="c.note" :disabled="!isNew" placeholder="why / what" /></Kv></div>
      <div><Kv k="Schedule publish at (empty = manual publish after approval)"><input class="in" type="datetime-local" v-model="sched" :disabled="!isNew" /></Kv><Kv k="Flow" :v="flow[c.state ?? 'draft']" /></div>
      <div v-if="!isNew"><Kv k="Author / reviewer" :v="`${c.author ?? '—'} / ${c.reviewer ?? '—'}${c.review_note ? ' — ' + c.review_note : ''}`" /><Kv k="Created · updated · published" :v="`${ts(c.created_at)} · ${ts(c.updated_at)} · ${ts(c.published_at)}${c.published_version ? ' (content #' + c.published_version + ')' : ''}`" /></div>
    </div>
    <template v-if="!isNew">
      <div style="margin-top:10px" v-if="validation"><span :class="validation.ok ? 'ok' : 'bad'">{{ validation.ok ? 'validation passed' : `validation: ${(validation.problems ?? []).length} problem(s)` }}</span>
        <ul class="dim" v-if="!validation.ok" style="margin:4px 0 0 16px"><li v-for="(p, i) in validation.problems ?? []" :key="i">{{ p.table ? p.table + ' / ' + (p.key ?? '') + ': ' : '' }}{{ p.problem ?? JSON.stringify(p) }}</li></ul></div>
      <div class="tabs" style="margin-top:12px"><a :class="{ on: tab === 'changes' }" @click="tab = 'changes'">Changes ({{ changes.length }})</a><a :class="{ on: tab === 'diff' }" @click="tab = 'diff'">Diff vs live</a><a :class="{ on: tab === 'preview' }" @click="loadPreview">Preview</a></div>
      <template v-if="tab === 'changes'">
        <table><thead><tr><th style="width:180px">Table</th><th style="width:90px">Op</th><th style="width:240px">Key</th><th>Row</th><th style="width:60px" v-if="editable"></th></tr></thead><tbody>
          <tr v-for="(ch, i) in changes" :key="i"><td class="m">{{ ch.table }}</td><td :class="{ bad: (ch.op ?? 'upsert') === 'delete' }">{{ ch.op ?? 'upsert' }}</td><td class="m">{{ keyOf(ch) }}</td><td class="m dim" style="white-space:pre-wrap">{{ JSON.stringify(ch.row ?? {}).slice(0, 300) }}</td><td v-if="editable"><button class="btn s" @click="act(() => api('DELETE', `${C}/changes/${ch.table}/${encodeURIComponent(keyOf(ch))}`), 'unstaged')">×</button></td></tr>
          <tr v-if="!changes.length"><td colspan="5" class="empty">Nothing staged — add rows to upsert or delete, then submit</td></tr></tbody></table>
        <div v-if="c.counts" class="dim" style="margin-top:8px">{{ Object.entries(c.counts).map(([t, v]: [string, any]) => `${t}: +${v.upserts ?? 0} −${v.deletes ?? 0}`).join(' · ') }}</div>
      </template>
      <template v-else-if="tab === 'diff'"><p class="note" v-if="diff && diff.changes === false">No difference against the live tables (already published or empty).</p><JsonView :value="diff?.diff ?? diff ?? { note: 'no diff available' }" /></template>
      <template v-else><p class="note">The economy tables as they would look after publishing.</p><JsonView :value="preview ?? {}" /></template>
    </template>
  </div>
  <Drawer v-if="dlg === 'stage'" title="Stage a change" @close="dlg = ''"><div class="form">
    <div class="f"><label>Table</label><select class="sel" v-model="form.table"><option v-for="t in tables" :key="t">{{ t }}</option></select></div>
    <div class="f"><label>Operation</label><select class="sel" v-model="form.op"><option value="upsert">upsert — insert or replace the row</option><option value="delete">delete — remove the row with this key</option></select></div>
    <div class="f"><label>Row (JSON — for delete only the key field is needed)</label><textarea class="in m" v-model="form.row" style="min-height:200px"></textarea></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="stage">Stage</button></div></div></Drawer>
  <Drawer v-if="dlg === 'approve' || dlg === 'reject'" :title="dlg === 'approve' ? 'Approve changeset' : 'Reject changeset'" @close="dlg = ''"><div class="form">
    <div class="f"><label>Review note</label><input class="in" v-model="form.note" /></div>
    <div class="row"><button class="btn" @click="dlg = ''">Cancel</button><button class="btn p" @click="act(() => post(`${C}/${dlg}`, { note: form.note }), dlg === 'approve' ? 'approved' : 'rejected')">{{ dlg === 'approve' ? 'Approve' : 'Reject' }}</button></div></div></Drawer>
</template>
