<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get, post, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const ask = (m: string) => window.confirm(m)
const id = computed(() => String(route.params.id)); const isNew = computed(() => id.value === 'new'); const N = computed(() => `/v1/server/news/${encodeURIComponent(id.value)}`)
const n = ref<any>(null); const err = ref(''); const stats = ref<any>(null); const audiences = ref<string[]>([]); const tags = ref(''); const pub = ref(''); const exp = ref(''); const meta = ref('{}')
interface L { locale: string; title: string; body: string; saved: boolean }
const locales = ref<L[]>([]); const newLocale = ref('')
const toIso = (v: any) => v ? new Date(v * 1000).toISOString().slice(0, 16) : ''
const fromIso = (x: string) => x ? Math.floor(new Date(x).getTime() / 1000) : null
async function load() {
  err.value = ''
  try {
    if (isNew.value) n.value = { title: '', body: '', pinned: false, enabled: true, link: null, image: null, audience: null, locales: {}, metadata: {} }
    else { const r = await get(N.value); n.value = r.news ?? r.item ?? r; stats.value = await get(`${N.value}/stats`).catch(() => null) }
    tags.value = (n.value.tags ?? []).join(', '); pub.value = toIso(n.value.published_at); exp.value = toIso(n.value.expires_at); meta.value = JSON.stringify(n.value.metadata ?? {}, null, 2)
    locales.value = Object.entries(n.value.locales ?? {}).map(([locale, v]: [string, any]) => ({ locale, title: v.title ?? '', body: v.body ?? '', saved: true }))
  } catch (x: any) { err.value = x.message }
  try { audiences.value = ((await get('/v1/server/audiences')).audiences ?? []).map((a: any) => a.name) } catch {}
}
onMounted(load)
const state = computed(() => { if (!n.value) return ''; if (!n.value.enabled) return 'disabled'; const t = Date.now() / 1000; if (n.value.published_at && t < n.value.published_at) return 'scheduled'; if (n.value.expires_at && t > n.value.expires_at) return 'expired'; return 'published' })
function body() { const { locales: _l, ...rest } = n.value; return { ...rest, tags: tags.value.split(',').map((s) => s.trim()).filter(Boolean), published_at: fromIso(pub.value), expires_at: fromIso(exp.value), metadata: JSON.parse(meta.value || '{}'), audience: n.value.audience || null, link: n.value.link || null, image: n.value.image || null } }
async function save() { try { const b = body(); const r = isNew.value ? await post('/v1/server/news', b) : await put(N.value, b); toast.show('saved'); if (isNew.value) router.replace(`/liveops/news/${encodeURIComponent(r.id ?? r.news?.id)}`); else load() } catch (x: any) { toast.err(x) } }
async function saveLocale(l: L) { try { await put(`${N.value}/locales/${encodeURIComponent(l.locale)}`, { title: l.title, body: l.body }); l.saved = true; toast.show(`${l.locale} saved`) } catch (x: any) { toast.err(x) } }
async function dropLocale(l: L, i: number) { if (!l.saved) { locales.value.splice(i, 1); return } try { await api('DELETE', `${N.value}/locales/${encodeURIComponent(l.locale)}`); locales.value.splice(i, 1); toast.show('removed') } catch (x: any) { toast.err(x) } }
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
</script>
<template>
  <div class="crumb"><router-link to="/liveops/news">News</router-link> › <span>{{ isNew ? 'new article' : n?.title || id }}</span><span class="id" v-if="!isNew">{{ id }} · {{ state }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s" v-if="!isNew" @click="ask('Delete this article?') && api('DELETE', N).then(() => router.push('/liveops/news'))">Delete</button><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="n">
    <div class="grid" style="grid-template-columns:2fr 1fr">
      <div><Kv k="Title"><input class="in" v-model="n.title" /></Kv><Kv k="Body (markdown as the client renders it)"><textarea class="in" v-model="n.body" style="min-height:180px"></textarea></Kv><Kv k="Link / image URL"><input class="in m" v-model="n.link" placeholder="https://…" /><input class="in m" v-model="n.image" placeholder="https://…/banner.png" style="margin-top:6px" /></Kv></div>
      <div><Kv k="Publish at (empty = now)"><input class="in" type="datetime-local" v-model="pub" /></Kv><Kv k="Expires at"><input class="in" type="datetime-local" v-model="exp" /></Kv><Kv k="Audience"><select class="sel" v-model="n.audience"><option :value="null">everyone</option><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option></select></Kv><Kv k="Tags (comma-separated)"><input class="in" v-model="tags" placeholder="patch, event" /></Kv><Kv k="Flags"><label><input type="checkbox" v-model="n.pinned" /> pinned to the top</label><br /><label><input type="checkbox" v-model="n.enabled" /> enabled</label></Kv>
        <template v-if="stats"><Kv k="Reads" :v="stats.reads ?? 0" /><Kv k="First / last read" :v="`${ts(stats.first_read_at)} · ${ts(stats.last_read_at)}`" /></template></div>
    </div>
    <template v-if="!isNew">
      <h3>Localisations <small>({{ locales.length }} — clients get their locale, else the default above)</small> <span style="margin-left:8px;display:inline-flex;gap:6px"><input class="in m" v-model="newLocale" placeholder="de, pt-BR" style="width:110px" /><button class="btn s p" :disabled="!newLocale.trim()" @click="locales.push({ locale: newLocale.trim(), title: '', body: '', saved: false }); newLocale = ''">Add</button></span></h3>
      <table><thead><tr><th style="width:90px">Locale</th><th style="width:30%">Title</th><th>Body</th><th style="width:140px"></th></tr></thead><tbody>
        <tr v-for="(l, i) in locales" :key="l.locale"><td class="m">{{ l.locale }}</td><td><input class="in" style="min-width:0;width:100%" v-model="l.title" @input="l.saved = false" /></td><td><textarea class="in" style="min-width:0;width:100%;min-height:60px" v-model="l.body" @input="l.saved = false"></textarea></td><td><button class="btn s p" :disabled="l.saved" @click="saveLocale(l)">Save</button> <button class="btn s" @click="dropLocale(l, i)">×</button></td></tr>
        <tr v-if="!locales.length"><td colspan="4" class="empty">Default language only</td></tr></tbody></table>
    </template>
    <h3>Metadata (JSON)</h3><textarea class="in m" v-model="meta" style="min-height:70px"></textarea>
    <h3 v-if="!isNew">Raw</h3><JsonView v-if="!isNew" :value="n" />
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
