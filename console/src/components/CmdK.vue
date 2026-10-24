<script setup lang="ts">
// Global search (Ctrl/⌘-K): pages by title, then objects from the node — accounts, worlds,
// matches, nodes, operator keys — one request per keystroke, Enter opens the highlighted hit.
import { ref, computed, watch, nextTick } from 'vue'
import { useRouter } from 'vue-router'
import { get, qs } from '../api/client'
import { sections } from '../router'
import { catalog } from '../catalog'
interface Hit { group: string; title: string; sub?: string; to: string }
const open = ref(false); const q = ref(''); const hits = ref<Hit[]>([]); const sel = ref(0); const busy = ref(false); const err = ref('')
const input = ref<HTMLInputElement | null>(null); const router = useRouter()
const pages = computed<Hit[]>(() => {
  const out: Hit[] = [{ group: 'pages', title: 'Overview', to: '/' }]
  for (const s of sections) for (const r of (catalog as any)[s.id] ?? []) out.push({ group: 'pages', title: `${s.title} › ${r.title}`, to: `/${s.id}/${r.id}` })
  return out
})
const local = computed(() => {
  const t = q.value.trim().toLowerCase(); if (!t) return pages.value.slice(0, 8)
  return pages.value.filter((p) => p.title.toLowerCase().includes(t)).slice(0, 6)
})
let timer: ReturnType<typeof setTimeout> | null = null
watch(q, () => { sel.value = 0; if (timer) clearTimeout(timer); timer = setTimeout(remote, 150) })
async function remote() {
  const t = q.value.trim(); if (t.length < 2) { hits.value = []; return }
  busy.value = true; err.value = ''
  try {
    const r = await get('/v1/admin/search' + qs({ q: t, limit: 8 }))
    const out: Hit[] = []
    for (const a of r.accounts ?? []) out.push({ group: 'players', title: a.username || a.display_name || a.id, sub: a.id, to: `/players/accounts/${a.id}` })
    for (const w of r.worlds ?? []) out.push({ group: 'worlds', title: w.name, sub: w.node_id ? `node ${w.node_id}` : 'no node', to: `/world/worlds/${encodeURIComponent(w.name)}` })
    for (const m of r.matches ?? []) out.push({ group: 'matches', title: m.label || m.match_id, sub: `${m.module} · tick ${m.tick}`, to: `/cluster/matches/${m.match_id}` })
    for (const n of r.nodes ?? []) out.push({ group: 'nodes', title: n.name, sub: `${n.region} · ${n.alive ? 'alive' : 'down'}`, to: `/cluster/nodes/${n.id}` })
    for (const k of r.keys ?? []) out.push({ group: 'operator keys', title: k.name, sub: `${k.role}${k.revoked_at ? ' · revoked' : ''}`, to: `/settings/keys/${k.id}` })
    if (r.entity) out.push({ group: 'entities', title: r.entity.id, sub: `world ${r.entity.world}`, to: `/world/worlds/${encodeURIComponent(r.entity.world)}/entities/${r.entity.id}` })
    hits.value = out
  } catch (e: any) { err.value = e.message; hits.value = [] }
  busy.value = false
}
const all = computed(() => [...local.value, ...hits.value])
function show() { open.value = true; q.value = ''; hits.value = []; sel.value = 0; nextTick(() => input.value?.focus()) }
function go(h?: Hit) { const x = h ?? all.value[sel.value]; if (!x) return; open.value = false; router.push(x.to) }
function key(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); open.value ? (open.value = false) : show(); return }
  if (!open.value) return
  if (e.key === 'Escape') { open.value = false }
  else if (e.key === 'ArrowDown') { e.preventDefault(); sel.value = Math.min(all.value.length - 1, sel.value + 1) }
  else if (e.key === 'ArrowUp') { e.preventDefault(); sel.value = Math.max(0, sel.value - 1) }
  else if (e.key === 'Enter') { e.preventDefault(); go() }
}
window.addEventListener('keydown', key)
defineExpose({ show })
</script>
<template>
  <div class="cmdk" v-if="open" @click.self="open = false">
    <div class="box">
      <input ref="input" class="in" v-model="q" placeholder="search players, worlds, matches, nodes, keys, pages…" autofocus />
      <div class="list">
        <template v-for="(h, i) in all" :key="h.to + i">
          <div class="grp" v-if="i === 0 || all[i - 1].group !== h.group">{{ h.group }}</div>
          <a :class="{ on: i === sel }" @mouseenter="sel = i" @click="go(h)"><span class="t">{{ h.title }}</span><span class="s m" v-if="h.sub">{{ h.sub }}</span></a>
        </template>
        <div class="dim" v-if="busy" style="padding:8px 12px;font-size:12px">searching…</div>
        <div class="err" v-else-if="err" style="margin:8px 12px">{{ err }}</div>
        <div class="dim" v-else-if="q.trim().length >= 2 && !hits.length" style="padding:8px 12px;font-size:12px">no objects match “{{ q.trim() }}”</div>
      </div>
      <div class="foot dim">↑↓ move · Enter open · Esc close</div>
    </div>
  </div>
</template>
<style scoped>
.cmdk { position: fixed; inset: 0; background: rgba(0,0,0,.45); z-index: 60; display: flex; justify-content: center; align-items: flex-start; padding: 10vh 12px 0 }
.box { width: min(640px, 100%); background: var(--bg2, #16181d); border: 1px solid var(--line, #2a2d35); border-radius: 10px; box-shadow: 0 20px 60px rgba(0,0,0,.5); overflow: hidden }
.box .in { width: 100%; box-sizing: border-box; border: 0; border-bottom: 1px solid var(--line, #2a2d35); border-radius: 0; padding: 14px; font-size: 15px; min-width: 0 }
.list { max-height: 55vh; overflow: auto }
.grp { font-size: 11px; text-transform: uppercase; letter-spacing: .06em; opacity: .55; padding: 8px 12px 2px }
.list a { display: flex; gap: 12px; align-items: baseline; padding: 7px 12px; cursor: pointer; text-decoration: none; color: inherit; min-width: 0 }
.list a.on { background: rgba(122,162,255,.14) }
.list .t { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap }
.list .s { font-size: 11px; opacity: .6; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 45% }
.foot { font-size: 11px; padding: 6px 12px; border-top: 1px solid var(--line, #2a2d35) }
</style>
