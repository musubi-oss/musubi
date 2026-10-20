<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { get, qs } from '../../api/client'
const rows = ref<any[]>([]); const err = ref(''); const cursor = ref<string | null>(null); const loading = ref(false); const open = ref<string | null>(null)
const f = ref({ actor: '', method: '', path: '', since: '', status: '' })
async function load(more = false) {
  loading.value = true; err.value = ''
  try {
    const r = await get('/v1/admin/audit' + qs({ limit: 100, cursor: more ? cursor.value : undefined, actor: f.value.actor || undefined, path_prefix: f.value.path || undefined }))
    const since = f.value.since ? new Date(f.value.since).getTime() : 0
    const list = (r.entries ?? []).filter((e: any) => (!f.value.status || String(e.status).startsWith(f.value.status)) && (!f.value.method || e.method === f.value.method) && (!since || new Date(e.at).getTime() >= since))
    rows.value = more ? [...rows.value, ...list] : list; cursor.value = r.cursor ?? null
  } catch (e: any) { err.value = e.message } finally { loading.value = false }
}
onMounted(() => load())
const ts = (v: any) => v ? String(v).replace('T', ' ').slice(0, 19) : '—'
</script>
<template>
  <div class="crumb"><span>Audit log</span><span class="id">{{ rows.length }} loaded{{ cursor ? ' · more available' : '' }} · actor and path filter server-side, method / since / status filter the loaded page</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <div class="filters" style="display:flex;gap:8px;flex-wrap:wrap;margin-bottom:10px">
      <input class="in" v-model="f.actor" placeholder="actor (key name / operator)" style="width:200px" @keyup.enter="load()" />
      <select class="sel" v-model="f.method" @change="load()"><option value="">any method</option><option>POST</option><option>PUT</option><option>PATCH</option><option>DELETE</option><option>GET</option></select>
      <input class="in m" v-model="f.path" placeholder="path prefix, e.g. /v1/server/accounts" style="width:260px" @keyup.enter="load()" />
      <input class="in" type="datetime-local" v-model="f.since" @change="load()" />
      <select class="sel" v-model="f.status" @change="load()"><option value="">any status</option><option value="2">2xx</option><option value="4">4xx</option><option value="5">5xx</option></select>
      <button class="btn s" @click="load()">Apply</button><button class="btn s" @click="f = { actor: '', method: '', path: '', since: '', status: '' }; load()">Reset</button>
    </div>
    <table><thead><tr><th style="width:150px">At</th><th style="width:160px">Actor</th><th style="width:70px">Method</th><th>Path</th><th style="width:60px">Status</th><th style="width:70px">ms</th><th style="width:120px">IP</th></tr></thead><tbody>
      <template v-for="e in rows" :key="e.id">
        <tr class="row" @click="open = open === e.id ? null : e.id"><td class="dim">{{ ts(e.at) }}</td><td>{{ e.actor }}</td><td class="m">{{ e.method }}</td><td class="m">{{ e.path }}</td><td :class="{ bad: e.status >= 400 }">{{ e.status }}</td><td class="dim">{{ e.duration_ms }}</td><td class="dim m">{{ e.ip || '—' }}</td></tr>
        <tr v-if="open === e.id"><td colspan="7"><pre class="pre" style="margin:4px 0">{{ JSON.stringify({ id: e.id, route: e.route, body: e.body }, null, 2) }}</pre></td></tr>
      </template>
      <tr v-if="!rows.length && !loading"><td colspan="7" class="empty">No entries match</td></tr></tbody></table>
    <div class="row" style="margin-top:10px" v-if="cursor"><button class="btn s" :disabled="loading" @click="load(true)">Load more</button></div>
  </div>
</template>
