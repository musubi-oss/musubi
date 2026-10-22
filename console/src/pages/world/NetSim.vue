<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const s = ref<any>(null); const err = ref(''); const f = ref({ enabled: true, latency_ms: 80, jitter_ms: 20, loss_pct: 1, duplicate_pct: 0, reorder_pct: 0, bandwidth_kbps: 0 })
async function load() { err.value = ''; try { s.value = await get('/v1/admin/dev/netsim'); if (s.value.global) f.value = { ...f.value, ...s.value.global } } catch (x: any) { err.value = x.message } }
onMounted(load)
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
const presets = [['LTE', 60, 15, 0.5], ['3G', 200, 60, 2], ['Wi-Fi far', 30, 40, 1], ['Satellite', 600, 30, 1], ['Lossy', 80, 20, 10]] as const
</script>
<template>
  <div class="crumb"><span>Network simulator</span><span class="id" v-if="s">{{ s.active ? 'active' : 'inactive' }} · {{ s.config_enabled ? 'enabled in config' : 'disabled in [dev] config' }} · {{ (s.sessions ?? []).length }} shaped sessions</span><span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="act(() => post('/v1/admin/dev/netsim/reset'), 'counters reset')">Reset counters</button><button class="btn s" @click="act(() => api('DELETE', '/v1/admin/dev/netsim'), 'simulator disabled')">Disable</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="s">
    <p class="note">Shapes realtime / UDP traffic on this node for every session (or per session via the client header) — latency, jitter, loss, duplication, reordering, bandwidth cap. Dev tool: needs <code>[dev] netsim = true</code>.</p>
    <div class="grid"><div><Kv k="Latency (ms)"><input class="in" type="number" min="0" v-model.number="f.latency_ms" /></Kv><Kv k="Jitter (ms)"><input class="in" type="number" min="0" v-model.number="f.jitter_ms" /></Kv></div><div><Kv k="Loss (%)"><input class="in" type="number" min="0" max="100" step="0.1" v-model.number="f.loss_pct" /></Kv><Kv k="Duplicate (%)"><input class="in" type="number" min="0" max="100" step="0.1" v-model.number="f.duplicate_pct" /></Kv></div><div><Kv k="Reorder (%)"><input class="in" type="number" min="0" max="100" step="0.1" v-model.number="f.reorder_pct" /></Kv><Kv k="Bandwidth cap (kbps, 0 = none)"><input class="in" type="number" min="0" v-model.number="f.bandwidth_kbps" /></Kv></div></div>
    <div class="row" style="margin:10px 0"><span class="dim">Presets:</span><button class="btn s" v-for="[n, l, j, p] in presets" :key="n" @click="f.latency_ms = l; f.jitter_ms = j; f.loss_pct = p">{{ n }}</button><button class="btn p" style="margin-left:auto" @click="act(() => put('/v1/admin/dev/netsim', { ...f, enabled: true }), 'simulator applied')">Apply globally</button></div>
    <h3>Shaped sessions</h3><JsonView :value="s.sessions ?? []" />
  </div>
</template>
