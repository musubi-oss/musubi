<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { get, post, qs } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const router = useRouter()
const items = ref<any[]>([]); const err = ref(''); const status = ref('pending'); const dlg = ref(false); const sel = ref<any>(null); const form = ref({ decision: 'approved', note: '' })
async function load() { err.value = ''; try { const r = await get('/v1/admin/ugc' + qs({ status: status.value === 'all' ? undefined : status.value, limit: 200 })); items.value = r.items ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
async function decide() { try { await post(`/v1/admin/ugc/${sel.value.id}`, form.value); toast.show(form.value.decision); dlg.value = false; load() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => !v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(v)).toISOString().replace('T', ' ').slice(0, 16)
const counts = computed(() => items.value.length)
const pg1 = usePager(() => items)
</script>
<template>
  <div class="crumb"><span>User-generated content</span><span class="id">{{ counts }} {{ status }}</span><span style="margin-left:auto"><select class="sel" v-model="status" @change="load()"><option value="pending">pending review</option><option value="approved">approved</option><option value="rejected">rejected</option><option value="reported">reported</option><option value="all">all</option></select></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Maps, skins, levels and other player uploads wait here until approved; rejected items are hidden from everyone but the owner. Reports re-queue approved items.</p>
    <table><thead><tr><th style="width:140px">Uploaded</th><th>Title</th><th style="width:100px">Kind</th><th>Owner</th><th style="width:90px">Size</th><th style="width:80px">Reports</th><th style="width:90px">Status</th><th style="width:120px"></th></tr></thead><tbody>
      <tr v-for="u in pg1.rows" :key="u.id"><td class="dim">{{ ts(u.created_at) }}</td><td class="row" @click="sel = u; form = { decision: 'approved', note: '' }; dlg = true">{{ u.title ?? u.name ?? u.id }}<div class="dim" style="font-size:11px">{{ u.description }}</div></td><td class="dim">{{ u.kind }}</td><td class="m dim row" @click="router.push(`/players/accounts/${u.owner_id}`)">{{ String(u.owner_id ?? '').slice(0, 8) }}…</td><td class="dim">{{ u.size ? (u.size / 1024).toFixed(0) + ' KB' : '—' }}</td><td :class="{ bad: u.reports > 0 }">{{ u.reports ?? 0 }}</td><td :class="{ ok: u.status === 'pending' }">{{ u.status }}</td><td><button class="btn s p" @click="sel = u; form = { decision: 'approved', note: '' }; dlg = true">Review</button></td></tr>
      <tr v-if="!items.length"><td colspan="8" class="empty">Nothing {{ status === 'all' ? '' : status }}</td></tr></tbody></table><Pager :p="pg1" />
  </div>
  <Drawer v-if="dlg" :title="`Review ${sel.title ?? sel.id}`" @close="dlg = false"><div class="form">
    <a v-if="sel.url ?? sel.download_url" class="btn s" :href="sel.url ?? sel.download_url" target="_blank" rel="noopener">Open file</a>
    <JsonView :value="sel" />
    <div class="f"><label>Decision</label><select class="sel" v-model="form.decision"><option value="approved">approve — visible to everyone</option><option value="rejected">reject — hidden, owner notified</option><option value="removed">remove — delete the file</option></select></div>
    <div class="f"><label>Note to the owner</label><input class="in" v-model="form.note" /></div>
    <div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" @click="decide">Submit</button></div></div></Drawer>
</template>
