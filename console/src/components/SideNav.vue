<script setup lang="ts">
// Section sidebar: grouped catalog entries, highlight follows redirects (e.g. bans → reports?tab=bans),
// filter box when the list is long. Extra entries (hand-written pages) come via `extra`.
import { computed, ref } from 'vue'
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
function on(e: Entry) {
  const t = target(e.to); const cur = current.value
  if (t.includes('?')) return cur === t
  if (cur.startsWith(t + '?')) return !items.value.some((o) => o !== e && target(o.to) === cur)
  return cur === t || (cur.startsWith(t + '/') && !items.value.some((o) => o !== e && cur.startsWith(target(o.to) + '/')))
}
const long = computed(() => (catalog[props.section]?.length ?? 0) + (props.extra?.length ?? 0) > 12)
</script>
<template>
  <nav class="side">
    <input v-if="long" class="in nf" v-model="q" placeholder="find page" />
    <template v-for="(list, g) in groups" :key="g">
      <div class="grp" v-if="g">{{ g }}</div>
      <router-link v-for="it in list" :key="it.id" :to="it.to" :class="{ on: on(it) }">{{ it.title }}</router-link>
    </template>
    <div class="note" v-if="!items.length" style="padding:8px 10px">nothing matches</div>
  </nav>
</template>
