<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const arts = ref<any[]>([]); const pending = ref<any[]>([]); const chunking = ref<any>(null); const err = ref(''); const open = ref(''); const manifest = ref<any>(null); const sel = ref<any>(null)
async function load() { err.value = ''; try { const r = await get('/v1/server/builds'); arts.value = r.artifacts ?? []; pending.value = r.pending_uploads ?? []; chunking.value = await get('/v1/server/builds/chunking').catch(() => null) } catch (x: any) { err.value = x.message } }
onMounted(load)
const groups = computed(() => { const m = new Map<string, any[]>(); for (const a of arts.value) m.set(a.name, [...(m.get(a.name) ?? []), a]); return [...m.entries()].map(([name, vs]) => ({ name, vs: vs.sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0)) })) })
const mb = (n: any) => n == null ? '—' : n > 1048576 ? (n / 1048576).toFixed(1) + ' MB' : (n / 1024).toFixed(0) + ' KB'
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
async function showManifest(a: any) { sel.value = a; try { manifest.value = await api('GET', `/v1/server/builds/${encodeURIComponent(a.name)}/${encodeURIComponent(a.version)}/manifest`) } catch (x: any) { toast.err(x) } }
async function act(fn: () => Promise<any>, ok: string) { try { await fn(); toast.show(ok); load() } catch (x: any) { toast.err(x) } }
</script>
<template>
  <div class="crumb"><span>Builds &amp; artifacts</span><span class="id">{{ groups.length }} builds · {{ arts.length }} versions<template v-if="chunking"> · dedup {{ chunking.artifacts?.dedup_ratio ?? chunking.dedup_ratio ?? '—' }}× · {{ chunking.chunks?.orphans ?? chunking.orphans ?? 0 }} orphan chunks</template></span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" @click="act(() => post('/v1/server/builds/gc'), 'chunk gc done')">GC chunks</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Upload from CI with <code>musubi build push name@version file</code> (delta: only changed chunks travel). Launchers fetch the manifest and pull missing chunks; versions share identical chunks.</p>
    <template v-for="g in groups" :key="g.name">
      <h3 class="row" @click="open = open === g.name ? '' : g.name" style="cursor:pointer">{{ open === g.name ? '▾' : '▸' }} <span class="m">{{ g.name }}</span> <small>({{ g.vs.length }} versions · latest {{ g.vs[0].version }} · {{ ts(g.vs[0].created_at) }})</small></h3>
      <table v-if="open === g.name"><thead><tr><th style="width:110px">Version</th><th style="width:100px">Size</th><th style="width:90px">Chunks</th><th>Delta from previous</th><th style="width:160px">sha256</th><th style="width:140px">Created</th><th style="width:200px"></th></tr></thead><tbody>
        <tr v-for="a in g.vs" :key="a.version"><td class="m">{{ a.version }}</td><td>{{ mb(a.size) }}</td><td class="dim">{{ a.chunked ? (a.delta?.chunks ?? a.chunks ?? '✓') : 'raw' }}</td><td class="dim">{{ a.delta ? `${a.delta.from} → ${a.delta.new_pct}% new (${a.delta.new_chunks} chunks, ${mb(a.delta.new_bytes)}), ${a.delta.reused_chunks} reused` : 'first version' }}</td><td class="m dim">{{ String(a.sha256 ?? '').slice(0, 16) }}…</td><td class="dim">{{ ts(a.created_at) }}</td>
          <td><button class="btn s" @click="showManifest(a)">Manifest</button> <a class="btn s" :href="a.download_url" target="_blank" rel="noopener">Download</a> <button class="btn s" @click="ask(`Delete ${a.name} ${a.version}? Chunks only it uses are freed.`) && act(() => api('DELETE', `/v1/server/builds/${encodeURIComponent(a.name)}/${encodeURIComponent(a.version)}`), 'deleted')">×</button></td></tr></tbody></table>
    </template>
    <p class="empty" v-if="!groups.length">No artifacts uploaded</p>
    <template v-if="pending.length"><h3>Pending uploads <small>({{ pending.length }} — delta sessions awaiting chunks / commit)</small></h3>
      <table><thead><tr><th>Build</th><th>Version</th><th>Size</th><th>Started</th><th></th></tr></thead><tbody><tr v-for="p in pending" :key="p.name + p.version"><td class="m">{{ p.name }}</td><td class="m">{{ p.version }}</td><td>{{ mb(p.size) }}</td><td class="dim">{{ ts(p.started_at) }}</td><td><button class="btn s" @click="ask('Cancel this upload and free its chunks?') && act(() => api('DELETE', `/v1/server/builds/${encodeURIComponent(p.name)}/${encodeURIComponent(p.version)}`), 'upload cancelled')">Cancel</button></td></tr></tbody></table></template>
    <template v-if="manifest && sel"><h3>Manifest — {{ sel.name }} {{ sel.version }} <small>({{ (manifest.chunks ?? []).length }} chunks)</small></h3>
      <div class="grid"><div><Kv k="File" :v="`${manifest.filename ?? sel.filename} · ${mb(manifest.size ?? sel.size)}`" /></div><div><Kv k="sha256" :v="manifest.sha256 ?? sel.sha256" mono /></div><div><Kv k="Chunk sizes" :v="`min ${mb(manifest.chunk_min ?? manifest.min)} · avg ${mb(manifest.chunk_avg ?? manifest.avg)} · max ${mb(manifest.chunk_max ?? manifest.max)}`" /></div></div>
      <JsonView :value="{ ...manifest, chunks: (manifest.chunks ?? []).slice(0, 20).concat((manifest.chunks ?? []).length > 20 ? [`… ${(manifest.chunks ?? []).length - 20} more`] : []) }" /></template>
    <template v-if="chunking"><h3>Chunk store</h3><JsonView :value="chunking" /></template>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
