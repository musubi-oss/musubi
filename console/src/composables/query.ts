// Keep filter state in the URL so a page can be reloaded or shared with its filters intact.
// useQueryRef('q') → a string ref mirrored to ?q=; useQueryObj(form, ['name', 'kind']) mirrors chosen keys of an object ref.
import { ref, watch, type Ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
function write(router: ReturnType<typeof useRouter>, route: ReturnType<typeof useRoute>, patch: Record<string, string | undefined>) {
  const q: Record<string, any> = { ...route.query }
  for (const [k, v] of Object.entries(patch)) { if (v === undefined || v === '' || v === null) delete q[k]; else q[k] = v }
  router.replace({ query: q })
}
export function useQueryRef(key: string, def = ''): Ref<string> {
  const route = useRoute(); const router = useRouter()
  const r = ref(String(route.query[key] ?? def))
  watch(r, (v) => write(router, route, { [key]: v === def ? undefined : v }))
  return r
}
export function useQueryObj<T extends Record<string, any>>(obj: Ref<T>, keys: (keyof T & string)[]) {
  const route = useRoute(); const router = useRouter()
  for (const k of keys) { const v = route.query[k]; if (typeof v === 'string' && v !== '') (obj.value as any)[k] = typeof obj.value[k] === 'number' ? Number(v) : typeof obj.value[k] === 'boolean' ? v === 'true' : v }
  watch(obj, (o) => { const p: Record<string, string | undefined> = {}; for (const k of keys) { const v = o[k]; p[k] = v === undefined || v === null || v === '' || v === false ? undefined : String(v) } write(router, route, p) }, { deep: true })
  return obj
}
