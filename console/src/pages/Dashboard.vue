<script setup lang="ts">
// Landing page: what an operator wants to know in the first ten seconds — is the cluster healthy,
// who is online, where the worlds and matches run, what needs attention, what changed last.
// Everything comes from endpoints the other pages already use; each card fails on its own.
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { get } from '../api/client'
import { useSession } from '../stores/session'
import Bars from '../components/Bars.vue'
import DataTable from '../components/DataTable.vue'
const s = useSession()
const x = ref<Record<string, any>>({}); const fail = ref<Record<string, string>>({}); const loading = ref(true); const at = ref<Date | null>(null)
async function fx(k: string, p: string) { try { x.value[k] = await get(p); delete fail.value[k] } catch (e: any) { fail.value[k] = e?.status === 403 ? 'not visible to your role' : (e?.message ?? String(e)) } }
async function load() {
  await Promise.all([
    fx('node', '/v1/node'), fx('nodes', '/v1/server/cluster/nodes'), fx('raft', '/v1/server/cluster/raft'),
    fx('worlds', '/v1/admin/cluster/worlds'), fx('matches', '/v1/admin/cluster/matches'), fx('fleets', '/v1/server/fleets'),
    fx('mm', '/v1/server/matchmaker/stats'), fx('au', '/v1/server/analytics/kpi/active_users?days=14'), fx('rev', '/v1/server/analytics/kpi/revenue?days=7'),
    fx('cr', '/v1/server/crashes/summary'), fx('audit', '/v1/admin/audit?limit=12'), fx('sanctions', '/v1/admin/sanctions?limit=6'), fx('hooks', '/v1/admin/webhooks'),
  ])
  loading.value = false; at.value = new Date()
}
let timer: ReturnType<typeof setInterval> | null = null
onMounted(() => { load(); timer = setInterval(load, 15000) }); onBeforeUnmount(() => { if (timer) clearInterval(timer) })

const nodes = computed(() => (x.value.nodes?.nodes ?? []).map((n: any) => ({ ...n, me: n.id === x.value.nodes?.self })))
const alive = computed(() => nodes.value.filter((n: any) => n.alive).length)
const worlds = computed<any[]>(() => x.value.worlds?.worlds ?? [])
const localWorlds = computed<string[]>(() => x.value.worlds?.local ?? [])
const unplaced = computed(() => worlds.value.filter((w: any) => !w.node_id || !nodes.value.some((n: any) => n.id === w.node_id && n.alive)))
const matches = computed<any[]>(() => x.value.matches?.matches ?? [])
const fleets = computed<any[]>(() => x.value.fleets?.fleets ?? [])
const queues = computed(() => { const m = x.value.mm ?? {}; const names = new Set<string>([...(m.queues ?? []), ...Object.keys(m.waiting ?? {})]); return [...names].map((q) => ({ queue: q, waiting: m.waiting?.[q] ?? 0, oldest: m.waiting_ages?.[q] ?? null, paused: (m.paused ?? []).includes(q), draining: (m.draining ?? []).includes(q) })) })
const waiting = computed(() => Object.values<number>(x.value.mm?.waiting ?? {}).reduce((a, b) => a + b, 0))
const revenue = computed(() => (x.value.rev?.daily ?? []).reduce((a: number, d: any) => a + d.amount, 0))
const purchases = computed(() => (x.value.rev?.daily ?? []).reduce((a: number, d: any) => a + d.purchases, 0))
const hooksBad = computed(() => (x.value.hooks?.webhooks ?? []).filter((h: any) => h.disabled || (h.failures ?? h.consecutive_failures ?? 0) > 0))
const uptime = computed(() => { const u = x.value.node?.uptime_secs; if (u == null) return '—'; return u < 3600 ? `${Math.round(u / 60)} min` : u < 86400 ? `${(u / 3600).toFixed(1)} h` : `${(u / 86400).toFixed(1)} d` })

// things an operator should act on, worst first
const attention = computed(() => {
  const out: { level: 'bad' | 'warn' | 'dim'; text: string; to: string }[] = []
  const dead = nodes.value.filter((n: any) => !n.alive)
  if (dead.length) out.push({ level: 'bad', text: `${dead.length} node${dead.length > 1 ? 's' : ''} not heartbeating: ${dead.map((n: any) => n.name).join(', ')}`, to: '/cluster' })
  if (unplaced.value.length) out.push({ level: 'bad', text: `${unplaced.value.length} world${unplaced.value.length > 1 ? 's' : ''} without a live node`, to: '/world' })
  if (x.value.cr && x.value.cr.crash_free_users_pct < 99) out.push({ level: 'warn', text: `crash-free users at ${x.value.cr.crash_free_users_pct.toFixed(1)}% over 24 h (${x.value.cr.crashes} crashes)`, to: '/analytics/crashes' })
  if (hooksBad.value.length) out.push({ level: 'warn', text: `${hooksBad.value.length} webhook${hooksBad.value.length > 1 ? 's' : ''} failing or disabled`, to: '/settings/webhooks' })
  for (const q of queues.value) if (q.paused) out.push({ level: 'warn', text: `matchmaking queue "${q.queue}" is paused`, to: '/social/matchmaking' })
  if (nodes.value.length > 1 && x.value.raft && !x.value.raft.enabled) out.push({ level: 'warn', text: 'several nodes but Raft is off — no shared directory / leader', to: '/cluster' })
  const fail403 = Object.entries(fail.value).filter(([, v]) => v !== 'not visible to your role')
  for (const [k, v] of fail403) out.push({ level: 'dim', text: `could not load ${k}: ${v}`, to: '/cluster' })
  return out
})
function ago(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : +new Date(v); const sec = Math.round((Date.now() - d) / 1000); return sec < 60 ? `${sec}s ago` : sec < 3600 ? `${Math.round(sec / 60)}m ago` : sec < 86400 ? `${Math.round(sec / 3600)}h ago` : `${Math.round(sec / 86400)}d ago` }
const short = (id: any) => (id ? String(id).slice(0, 8) : '—')
const nodeName = (id?: string | null) => (id ? nodes.value.find((n: any) => n.id === id)?.name ?? short(id) : '—')
const pct = (v?: number) => v == null ? '—' : (v * 100).toFixed(0) + '%'
</script>
<template>
  <div class="page dash">
    <div class="head"><h1>{{ x.node?.project ?? 'musubi' }} <small class="dim" v-if="x.node?.environment">{{ x.node.environment }}</small></h1><span class="sub">{{ x.node?.name ?? '' }} · {{ x.node?.region ?? '' }} · v{{ x.node?.version ?? '?' }} · up {{ uptime }}<span v-if="at"> · refreshed {{ at.toTimeString().slice(0, 8) }}</span></span></div>
    <div class="stats">
      <router-link class="stat" to="/cluster"><div class="k">Nodes</div><div class="v" :class="{ bad: nodes.length && alive < nodes.length }">{{ alive }}<small>/ {{ nodes.length }} alive · {{ x.raft?.enabled ? 'raft on' : 'single-node directory' }}</small></div></router-link>
      <router-link class="stat" to="/players"><div class="k">Online now</div><div class="v">{{ x.node?.realtime_sessions ?? '—' }}<small>realtime sessions on this node · {{ x.node?.parties ?? 0 }} parties</small></div></router-link>
      <router-link class="stat" to="/world"><div class="k">Worlds</div><div class="v">{{ worlds.length || localWorlds.length }}<small>{{ localWorlds.length }} on this node<template v-if="unplaced.length"> · <b class="bad">{{ unplaced.length }} unplaced</b></template></small></div></router-link>
      <router-link class="stat" to="/cluster/matches"><div class="k">Matches</div><div class="v">{{ matches.length }}<small>{{ x.node?.authoritative_matches ?? 0 }} authoritative · {{ x.node?.relayed_matches ?? 0 }} relayed here</small></div></router-link>
      <router-link class="stat" to="/social/matchmaking"><div class="k">Matchmaking</div><div class="v">{{ waiting }}<small>waiting in {{ queues.length }} queue{{ queues.length === 1 ? '' : 's' }} · {{ x.mm?.open_matches ?? 0 }} open matches</small></div></router-link>
      <router-link class="stat" to="/analytics"><div class="k">DAU / MAU</div><div class="v">{{ x.au?.dau ?? '—' }}<small>/ {{ x.au?.mau ?? '—' }} · stickiness {{ pct(x.au?.stickiness) }}</small></div></router-link>
      <router-link class="stat" to="/analytics"><div class="k">Revenue 7 d</div><div class="v">{{ revenue }}<small>{{ purchases }} purchases · all currencies</small></div></router-link>
      <router-link class="stat" to="/analytics/crashes"><div class="k">Crash-free 24 h</div><div class="v" :class="{ warn: x.cr && x.cr.crash_free_users_pct < 99 }">{{ x.cr ? x.cr.crash_free_users_pct.toFixed(1) + '%' : '—' }}<small>{{ x.cr?.crashes ?? 0 }} crashes · {{ x.cr?.active_users ?? 0 }} users seen</small></div></router-link>
    </div>

    <div class="grid2">
      <div class="card"><h3 style="margin-top:0">Needs attention <small v-if="!loading">({{ attention.length }})</small></h3>
        <div v-if="loading" class="skel"><i></i><i></i><i></i></div>
        <p class="ok" v-else-if="!attention.length">Nothing — every node heartbeats, every world has a home, no failing webhooks, crash-free above 99%.</p>
        <ul class="att" v-else><li v-for="(a, i) in attention" :key="i" :class="a.level"><router-link :to="a.to">{{ a.text }}</router-link></li></ul>
      </div>
      <div class="card"><h3 style="margin-top:0">Active users <small>(14 days)</small></h3>
        <div v-if="loading" class="skel"><i></i><i></i></div>
        <template v-else-if="x.au"><Bars :values="(x.au.daily ?? []).map((d: any) => ({ label: d.day, v: d.users }))" /><div class="dim" style="font-size:12px;margin-top:6px">WAU {{ x.au.wau }} · {{ (x.au.daily ?? []).reduce((a: number, d: any) => a + d.events, 0) }} events</div></template>
        <p class="empty" v-else>{{ fail.au ?? 'no analytics yet' }}</p>
      </div>
    </div>

    <div class="grid2">
      <div class="card"><h3 style="margin-top:0">Cluster <small>({{ nodes.length }} node{{ nodes.length === 1 ? '' : 's' }})</small></h3>
        <div v-if="loading" class="skel"><i></i><i></i></div>
        <DataTable v-else :cols="[{ key: 'name', fmt: (r) => r.name + (r.me ? ' (this)' : '') }, { key: 'region', dim: true }, { key: 'roles', fmt: (r) => (r.roles ?? []).join(' · '), dim: true }, { key: 'alive', fmt: (r) => r.alive ? 'alive' : 'DOWN' }, { key: 'heartbeat_at', label: 'heartbeat', fmt: (r) => ago(r.heartbeat_at), dim: true }, { key: 'worlds', fmt: (r) => String(worlds.filter((w: any) => w.node_id === r.id).length) }, { key: 'matches', fmt: (r) => String(matches.filter((m: any) => m.node_id === r.id).length) }]" :rows="nodes" />
        <p class="empty" v-if="!loading && !nodes.length">{{ fail.nodes ?? 'no nodes registered' }}</p>
        <router-link to="/cluster" class="more">cluster overview →</router-link>
      </div>
      <div class="card"><h3 style="margin-top:0">Fleets <small>({{ fleets.length }})</small></h3>
        <div v-if="loading" class="skel"><i></i><i></i></div>
        <DataTable v-else :cols="[{ key: 'name' }, { key: 'region', dim: true }, { key: 'provider', dim: true }, { key: 'min_servers', label: 'min / max', fmt: (r) => `${r.min_servers} / ${r.max_servers}` }, { key: 'buffer' }, { key: 'created_at', fmt: (r) => ago(r.created_at), dim: true }]" :rows="fleets.slice(0, 8)" />
        <p class="empty" v-if="!loading && !fleets.length">{{ fail.fleets ?? 'No fleets yet — a fleet keeps N dedicated servers warm per region; create one under Game Servers → Fleets.' }}</p>
        <router-link to="/servers" class="more">game servers →</router-link>
      </div>
    </div>

    <div class="grid2">
      <div class="card"><h3 style="margin-top:0">Recent operator actions</h3>
        <div v-if="loading" class="skel"><i></i><i></i><i></i></div>
        <DataTable v-else :cols="[{ key: 'at', fmt: (r) => ago(r.at), dim: true }, { key: 'actor' }, { key: 'method', mono: true }, { key: 'path', mono: true }]" :rows="x.audit?.entries ?? []" />
        <p class="empty" v-if="!loading && !(x.audit?.entries ?? []).length">{{ fail.audit ?? 'no audited writes yet' }}</p>
        <router-link to="/settings/audit" class="more">audit log →</router-link>
      </div>
      <div class="card"><h3 style="margin-top:0">Latest sanctions</h3>
        <div v-if="loading" class="skel"><i></i><i></i></div>
        <DataTable v-else :cols="[{ key: 'created_at', fmt: (r) => ago(r.created_at), dim: true }, { key: 'action' }, { key: 'account_id', fmt: (r) => short(r.account_id), mono: true }, { key: 'message', fmt: (r) => r.message || r.reason || '' }, { key: 'created_by', dim: true }]" :rows="x.sanctions?.sanctions ?? []" />
        <p class="empty" v-if="!loading && !(x.sanctions?.sanctions ?? []).length">{{ fail.sanctions ?? 'no bans or mutes on record' }}</p>
        <router-link to="/settings/sanctions" class="more">sanctions & appeals →</router-link>
      </div>
    </div>

    <div class="card" v-if="matches.length"><h3 style="margin-top:0">Live matches <small>({{ matches.length }})</small></h3>
      <DataTable :cols="[{ key: 'label' }, { key: 'module', mono: true }, { key: 'match_id', fmt: (r) => short(r.match_id), mono: true }, { key: 'node_id', label: 'node', fmt: (r) => nodeName(r.node_id) }, { key: 'tick' }, { key: 'updated_at', fmt: (r) => ago(r.updated_at), dim: true }]" :rows="matches.slice(0, 10)" />
      <router-link to="/cluster/matches" class="more">all matches →</router-link>
    </div>
    <p class="dim" style="font-size:12px" v-if="s.me?.role && s.me.role !== 'admin'">Signed in as {{ s.me.role }} — cards your role cannot read are hidden.</p>
  </div>
</template>
