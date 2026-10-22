<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { get, post } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const router = useRouter()
const flags = ref<any[]>([]); const err = ref(''); const show = ref('open'); const dlg = ref(false); const sel = ref<any>(null); const note = ref('')
async function load() { err.value = ''; try { flags.value = (await get('/v1/server/economy/fraud/flags')).flags ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
const view = computed(() => flags.value.filter((f) => show.value === 'all' || (show.value === 'open') === !f.resolved_at))
const byRule = computed(() => { const m: Record<string, number> = {}; for (const f of view.value) m[f.rule] = (m[f.rule] ?? 0) + 1; return m })
async function resolve() { try { await post(`/v1/server/economy/fraud/flags/${sel.value.id}/resolve`, { note: note.value }); toast.show('resolved'); dlg.value = false; load() } catch (x: any) { toast.err(x) } }
const ts = (v: any) => v ? new Date(v * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—'
const pg1 = usePager(() => view)
</script>
<template>
  <div class="crumb"><span>Fraud flags</span><span class="id">{{ view.length }} {{ show }} · {{ Object.entries(byRule).map(([r, n]) => `${r} ${n}`).join(' · ') }}</span><span style="margin-left:auto"><select class="sel" v-model="show"><option value="open">open</option><option value="resolved">resolved</option><option value="all">all</option></select></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Economy rules (purchase velocity, refund ratio, trade loops, currency spikes) raise a flag when a player crosses the limit; <b>blocked</b> means the rule also froze the action until a moderator resolves it.</p>
    <table><thead><tr><th style="width:140px">Raised</th><th>Player</th><th style="width:140px">Rule</th><th style="width:110px">Count / limit</th><th style="width:90px">Blocked</th><th style="width:140px">Resolved</th><th style="width:100px"></th></tr></thead><tbody>
      <tr v-for="f in pg1.rows" :key="f.id"><td class="dim">{{ ts(f.created_at) }}</td><td class="m row" @click="router.push(`/players/accounts/${f.account_id}?tab=bans`)">{{ f.account_id }}</td><td>{{ f.rule }}</td><td :class="{ bad: f.count > f.limit }">{{ f.count }} / {{ f.limit }}</td><td :class="{ bad: f.blocked }">{{ f.blocked ? 'yes' : 'no' }}</td><td class="dim">{{ ts(f.resolved_at) }}</td><td><button class="btn s p" v-if="!f.resolved_at" @click="sel = f; note = ''; dlg = true">Resolve</button></td></tr>
      <tr v-if="!view.length"><td colspan="7" class="empty">No {{ show }} flags</td></tr></tbody></table><Pager :p="pg1" />
  </div>
  <Drawer v-if="dlg" title="Resolve fraud flag" @close="dlg = false"><div class="form"><p class="dim">{{ sel.rule }} · {{ sel.count }} / {{ sel.limit }}{{ sel.blocked ? ' · unblocks the player' : '' }}</p><div class="f"><label>Note</label><input class="in" v-model="note" placeholder="false positive / refunded / banned" /></div><div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" @click="resolve">Resolve</button></div></div></Drawer>
</template>
