<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { get, qs } from '../../api/client'
const nodes = ref<any[]>([]); const lines = ref<any[]>([]); const err = ref(''); const f = ref({ node: '', level: '', target: '', q: '', limit: 300 }); const follow = ref(false); let timer: any = null; let before: number | undefined
async function load(more = false) {
  err.value = ''
  try { const r = await get('/v1/admin/logs' + qs({ node: f.value.node || undefined, level: f.value.level || undefined, target: f.value.target || undefined, q: f.value.q || undefined, limit: f.value.limit, before: more ? before : undefined })); const got = r.lines ?? r.entries ?? r.logs ?? (Array.isArray(r) ? r : []); lines.value = more ? [...lines.value, ...got] : got; before = r.next_before ?? r.before ?? (got.length ? got[got.length - 1].seq ?? got[got.length - 1].ts : undefined) } catch (x: any) { err.value = x.message }
}
onMounted(async () => { try { nodes.value = (await get('/v1/admin/logs/nodes')).nodes ?? [] } catch {} load() })
onUnmounted(() => clearInterval(timer))
function toggleFollow() { follow.value = !follow.value; clearInterval(timer); if (follow.value) timer = setInterval(() => load(), 2000) }
const ts = (v: any) => !v ? '' : typeof v === 'number' ? new Date(v > 1e12 ? v : v * 1000).toISOString().slice(11, 23) : String(v).slice(11, 23)
const lv = (l: any) => String(l.level ?? '').toLowerCase()
</script>
<template>
  <div class="crumb"><span>Node logs</span><span class="id">in-memory ring buffer of each node · {{ lines.length }} lines</span>
    <span style="margin-left:auto;display:flex;gap:6px;flex-wrap:wrap"><select class="sel" v-model="f.node" @change="load()"><option value="">this node</option><option v-for="n in nodes" :key="n.id" :value="n.id">{{ n.name }}{{ n.me ? ' (me)' : '' }} · {{ n.region }}</option></select><select class="sel" v-model="f.level" @change="load()"><option value="">all levels</option><option>error</option><option>warn</option><option>info</option><option>debug</option><option>trace</option></select><input class="in m" v-model="f.target" placeholder="target (musubi_server::fleet)" style="width:200px" @keyup.enter="load()" /><input class="in" v-model="f.q" placeholder="text" style="width:160px" @keyup.enter="load()" /><button class="btn s" @click="load()">Search</button><button class="btn s" :class="{ p: follow }" @click="toggleFollow">{{ follow ? 'Following' : 'Follow' }}</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <pre class="pre" style="max-height:70vh;font-size:12px;line-height:1.5"><template v-for="(l, i) in lines" :key="i"><span class="dim">{{ ts(l.ts ?? l.at ?? l.time) }}</span> <span :class="{ bad: lv(l) === 'error', ok: lv(l) === 'warn' }" style="display:inline-block;width:46px">{{ lv(l).toUpperCase() }}</span> <span class="dim">{{ l.target }}</span> {{ l.message ?? l.msg ?? l.fields?.message ?? '' }}<span class="dim" v-if="l.fields && Object.keys(l.fields).filter((k) => k !== 'message').length"> {{ Object.entries(l.fields).filter(([k]) => k !== 'message').map(([k, v]) => `${k}=${JSON.stringify(v)}`).join(' ') }}</span>
</template><span class="empty" v-if="!lines.length">No lines match</span></pre>
    <div class="row" style="margin-top:8px"><button class="btn s" @click="load(true)" :disabled="!lines.length">Older</button></div>
  </div>
</template>
