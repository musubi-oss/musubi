<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, onMounted } from 'vue'
import { api, get, post } from '../../api/client'
const ask = (m: string) => window.confirm(m)
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const schedules = ref<any[]>([]); const problems = ref<any[]>([]); const sources = ref<any[]>([]); const err = ref('')
async function load() { err.value = ''; const r = await Promise.allSettled([get('/v1/admin/runtime/schedules'), get('/v1/server/config_sources')]); if (r[0].status === 'fulfilled') { schedules.value = r[0].value.schedules ?? []; problems.value = r[0].value.problems ?? [] } else err.value = (r[0] as any).reason.message; sources.value = r[1].status === 'fulfilled' ? r[1].value.sources ?? [] : [] }
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => !v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(v)).toISOString().replace('T', ' ').slice(0, 16)
const pg2 = usePager(() => schedules)
const pg1 = usePager(() => sources)
</script>
<template>
  <div class="crumb"><span>Schedules, timers &amp; config sources</span><span class="id">{{ schedules.length }} cron schedules · {{ sources.length }} external config sources</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="act(() => post('/v1/admin/runtime/timers/sweep'), 'timers swept')">Sweep timers</button><button class="btn s" @click="load">Refresh</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <h3>Cron schedules <small>(declared by runtime modules; run on the leader)</small></h3>
    <p class="err" v-for="(p, i) in problems" :key="i">{{ typeof p === 'string' ? p : JSON.stringify(p) }}</p>
    <table><thead><tr><th>Id</th><th>Module · function</th><th style="width:140px">Cron</th><th style="width:140px">Last run</th><th style="width:140px">Next run</th><th style="width:100px">Last result</th><th style="width:90px"></th></tr></thead><tbody>
      <tr v-for="s in pg2.rows" :key="s.id ?? s.name"><td class="m">{{ s.id ?? s.name }}</td><td class="dim">{{ s.module }} · {{ s.function ?? s.fn }}</td><td class="m">{{ s.cron }}</td><td class="dim">{{ ts(s.last_run_at ?? s.last_run) }}</td><td class="dim">{{ ts(s.next_run_at ?? s.next_run) }}</td><td :class="{ bad: s.last_error }">{{ s.last_error ? 'error' : s.last_run_at ? 'ok' : '—' }}</td><td><button class="btn s" @click="act(() => post(`/v1/admin/runtime/schedules/${encodeURIComponent(s.id ?? s.name)}/run`), 'triggered')">Run now</button></td></tr>
      <tr v-if="!pg2.total"><td colspan="7" class="empty">No schedules — modules declare them with <code>schedule("name", "cron", fn)</code></td></tr></tbody></table><Pager :p="pg2" />
    <h3>External config sources <small>(remote configs pulled from git / HTTP / S3)</small></h3>
    <table><thead><tr><th>Config</th><th>Source</th><th style="width:140px">Last sync</th><th style="width:100px">Status</th><th style="width:150px"></th></tr></thead><tbody>
      <tr v-for="s in pg1.rows" :key="s.name"><td class="m">{{ s.name }}</td><td class="m dim">{{ s.kind ?? '' }} {{ s.url ?? s.path ?? '' }}{{ s.ref ? ` @ ${s.ref}` : '' }}</td><td class="dim">{{ ts(s.last_sync_at ?? s.synced_at) }}</td><td :class="{ bad: s.last_error }">{{ s.last_error ? 'error' : 'ok' }}</td><td><button class="btn s" @click="act(() => post(`/v1/server/configs/${encodeURIComponent(s.name)}/source/sync`), 'synced')">Sync</button> <button class="btn s" @click="ask('Detach source? The config keeps its last value.') && act(() => api('DELETE', `/v1/server/configs/${encodeURIComponent(s.name)}/source`), 'detached')">Detach</button></td></tr>
      <tr v-if="!pg1.total"><td colspan="5" class="empty">No external sources — attach one from a remote config's page</td></tr></tbody></table><Pager :p="pg1" />
    <details v-if="schedules.length"><summary class="dim">Raw</summary><JsonView :value="schedules" /></details>
  </div>
</template>
