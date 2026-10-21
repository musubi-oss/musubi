<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { get } from '../../api/client'
const cfg = ref<any>(null); const err = ref(''); const q = ref(''); const open = ref<Record<string, boolean>>({})
onMounted(async () => { try { cfg.value = await get('/v1/admin/config') } catch (x: any) { err.value = x.message } })
function flat(o: any, pre = ''): [string, any][] { if (o && typeof o === 'object' && !Array.isArray(o)) return Object.entries(o).flatMap(([k, v]) => flat(v, pre ? `${pre}.${k}` : k)); return [[pre, o]] }
const rows = computed(() => flat(cfg.value?.config ?? cfg.value ?? {}).filter(([k, v]) => !q.value || k.toLowerCase().includes(q.value.toLowerCase()) || JSON.stringify(v).toLowerCase().includes(q.value.toLowerCase())))
const sections = computed(() => { const m = new Map<string, [string, any][]>(); for (const r of rows.value) { const s = r[0].split('.')[0]; m.set(s, [...(m.get(s) ?? []), r]) } return [...m.entries()] })
const show = (v: any) => v === null ? 'null' : typeof v === 'string' ? (v === '' ? '""' : v) : JSON.stringify(v)
const secret = (k: string) => /secret|key|password|token|license/i.test(k)
</script>
<template>
  <div class="crumb"><span>Node config</span><span class="id">effective configuration of this node (file + env + defaults) · {{ rows.length }} values</span><span style="margin-left:auto"><input class="in" v-model="q" placeholder="search key or value" style="width:260px" /></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="cfg">
    <p class="note">Read-only: change <code>musubi.toml</code> or <code>MUSUBI__SECTION__KEY</code> env vars and restart the node. Secret-looking values are shown as the server reports them (redacted).</p>
    <template v-for="[s, items] in sections" :key="s">
      <h3 class="row" style="cursor:pointer" @click="open[s] = !(open[s] ?? !!q)">{{ (open[s] ?? !!q) ? '▾' : '▸' }} <span class="m">[{{ s }}]</span> <small>({{ items.length }})</small></h3>
      <table v-if="open[s] ?? !!q"><tbody><tr v-for="[k, v] in items" :key="k"><td class="m" style="width:40%">{{ k.slice(s.length + 1) || k }}</td><td class="m" :class="{ dim: v === null || v === '' || secret(k) }">{{ show(v) }}</td></tr></tbody></table>
    </template>
  </div>
</template>
