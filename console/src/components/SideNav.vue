<script setup lang="ts">
// Section sidebar: grouped catalog entries, highlight follows redirects (e.g. bans → reports?tab=bans),
// filter box when the list is long. Extra entries (hand-written pages) come via `extra`.
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { catalog, type Res } from '../catalog'
const props = defineProps<{ section: string; extra?: { id: string; title: string; group?: string; to?: string }[] }>()
const route = useRoute(); const router = useRouter(); const q = ref('')
type Entry = { id: string; title: string; group?: string; to: string }
const items = computed<Entry[]>(() => {
  const base = (catalog[props.section] ?? []) as Res[]
  const list: Entry[] = base.map((it, i) => ({ id: it.id, title: it.title, group: it.group, to: i === 0 && !it.group ? `/${props.section}` : `/${props.section}/${it.id}` }))
  for (const e of props.extra ?? []) list.push({ id: e.id, title: e.title, group: e.group, to: e.to ?? `/${props.section}/${e.id}` })
  const s = q.value.trim().toLowerCase()
  return s ? list.filter((x) => x.title.toLowerCase().includes(s)) : list
})
const groups = computed(() => { const g: Record<string, Entry[]> = {}; for (const it of items.value) (g[it.group ?? ''] ??= []).push(it); return g })
const norm = (p: string) => p.replace(/\/+$/, '')
function target(to: string): string {
  const r = router.resolve(to); const rec = r.matched[r.matched.length - 1]
  const red = rec?.redirect; if (!red) return norm(r.fullPath)
  return norm(typeof red === 'string' ? red : typeof red === 'function' ? r.fullPath : router.resolve(red as any).fullPath)
}
const current = computed(() => { const base = norm(route.path); const tab = route.query.tab; return tab ? `${base}?tab=${tab}` : base })
// An entry's own path (redirects not followed): the page a direct link lands on.
const direct = (e: Entry) => norm(router.resolve(e.to).fullPath) === norm(route.path)
function matches(e: Entry, cur: string) {
  const t = target(e.to)
  if (cur === t) return true
  if (t.includes('?') || !(cur.startsWith(t + '/') || cur.startsWith(t + '?'))) return false
  return !items.value.some((o) => { if (o === e) return false; const ot = target(o.to); return ot.length > t.length && (cur === ot || cur.startsWith(ot + '/') || cur.startsWith(ot + '?')) })
}
// Several entries may redirect to the same page (allowlist/denials/login queue → access): when the page itself has an
// entry, only that one lights up; redirect-only entries highlight when the page has no entry of its own (bans → reports?tab=bans).
function on(e: Entry) {
  const cur = current.value
  if (!matches(e, cur)) return false
  const hit = items.value.filter((o) => matches(o, cur))
  return hit.some(direct) ? direct(e) : hit[0] === e
}
const open = ref(false)
const label = computed(() => items.value.find((e) => on(e))?.title ?? 'Pages')
watch(() => route.fullPath, () => { open.value = false })
const long = computed(() => (catalog[props.section]?.length ?? 0) + (props.extra?.length ?? 0) > 12)
</script>
<template>
  <nav class="side" :class="{ open }">
    <button class="tg" @click="open = !open">{{ label }}<i>{{ open ? '▲' : '▼ pages' }}</i></button>
    <input v-if="long" class="in nf" v-model="q" placeholder="find page" />
    <div class="ls">
      <template v-for="(list, g) in groups" :key="g">
        <div class="grp" v-if="g">{{ g }}</div>
        <router-link v-for="it in list" :key="it.id" :to="it.to" active-class="" exact-active-class="" :class="{ on: on(it) }">{{ it.title }}</router-link>
      </template>
      <div class="note" v-if="!items.length" style="padding:8px 10px">nothing matches</div>
    </div>
  </nav>
</template>
