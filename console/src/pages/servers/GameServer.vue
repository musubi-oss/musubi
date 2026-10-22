<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const id = computed(() => String(route.params.id))
const G = computed(() => `/v1/server/gameservers/${id.value}`)
const sv = ref<any>(null); const err = ref(''); const x = ref<Record<string, any>>({})
async function load() {
  err.value = ''
  try { sv.value = await get(G.value) } catch (e: any) { err.value = e.message; return }
  for (const [k, p] of [['egress', `${G.value}/egress`], ['reputation', `${G.value}/reputation`], ['metrics', `/v1/server/fleets/${sv.value.fleet_id}/servers/${id.value}/metrics`], ['logs', `/v1/server/fleets/${sv.value.fleet_id}/servers/${id.value}/logs?tail=200`]]) {
    try { x.value[k] = await get(p) } catch (e: any) { x.value[k] = e.status === 404 ? null : { error: e.message } }
  }
}
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); await load() } catch (e: any) { toast.err(e) } }
const ask = (m: string) => window.confirm(m)
function ts(v: any) { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(v); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
const logText = computed(() => { const l = x.value.logs; if (!l || l.error) return ''; if (typeof l === 'string') return l; return (l.lines ?? l.entries ?? []).map((e: any) => typeof e === 'string' ? e : `${e.at ?? ''} ${e.stream ?? ''} ${e.line ?? e.message ?? JSON.stringify(e)}`).join('\n') })
</script>
<template>
  <div class="crumb"><router-link to="/servers">Fleets</router-link> › <router-link v-if="sv" :to="`/servers/fleets/${sv.fleet_id}?tab=servers`">{{ sv.fleet || sv.fleet_id }}</router-link> › <span>{{ id }}</span>
    <span style="margin-left:auto;display:flex;gap:8px" v-if="sv">
      <button class="btn s" v-if="!sv.reserved_until" @click="act(() => post(`${G}/reserve`, { secs: 600 }), 'reserved 10 min')">Reserve</button>
      <button class="btn s" v-else @click="act(() => post(`${G}/unreserve`), 'unreserved')">Unreserve</button>
      <button class="btn s" @click="act(() => post(`/v1/server/fleets/${sv.fleet_id}/servers/${id}/logs/archive`), 'log snapshot requested')">Archive logs</button>
      <button class="btn s d" @click="ask('Terminate this game server?') && act(async () => { await api('DELETE', G); router.push(`/servers/fleets/${sv.fleet_id}?tab=servers`) }, 'terminated')">Terminate</button>
    </span>
  </div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="sv">
    <div class="grid">
      <div><Kv k="Server ID" :v="sv.id" mono copy /><Kv k="State" :v="sv.state" /><Kv k="Address" :v="sv.address ? `${sv.address}:${sv.port}` : '—'" mono copy /><Kv k="Ports" :v="sv.ports" mono /></div>
      <div><Kv k="Fleet / region / pool" :v="`${sv.fleet || sv.fleet_id} / ${sv.region || '—'} / ${sv.pool || '—'}`" /><Kv k="Build" :v="sv.build_version" mono /><Kv k="Provider / host" :v="`${sv.provider} / ${sv.hostname || sv.host_id || '—'}`" /><Kv k="PID / node" :v="`${sv.pid ?? '—'} / ${sv.node_id ?? '—'}`" mono /></div>
      <div><Kv k="Players / capacity" :v="`${sv.players ?? 0} / ${sv.capacity ?? '—'}`" /><Kv k="Created / ready" :v="`${ts(sv.created_at)} / ${ts(sv.ready_at)}`" /><Kv k="Reserved until" :v="sv.reserved_until ? ts(sv.reserved_until) : '—'" /><Kv k="Last health" :v="sv.last_health" /></div>
    </div>
    <h3>Allocation</h3><JsonView :value="sv.allocation ?? null" />
    <h3>Counters & lists</h3><JsonView :value="{ counters: sv.counters ?? {}, lists: sv.lists ?? {}, labels: sv.labels ?? {} }" />
    <h3>Metrics</h3><JsonView :value="x.metrics ?? {}" />
    <h3>Reputation & egress</h3><JsonView :value="{ reputation: x.reputation, egress: x.egress }" />
    <h3>Logs <small>(tail)</small></h3><pre class="json" style="max-height:480px">{{ logText || 'No Data' }}</pre>
    <h3>Exit</h3><div class="dim">{{ sv.exit_reason ?? '—' }} <span v-if="sv.terminated_at">· terminated {{ ts(sv.terminated_at) }}</span></div>
  </div>
</template>
