<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { get, post, api } from '../../api/client'
import { useSession } from '../../stores/session'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const router = useRouter(); const s = useSession()
const rows = ref<any[]>([]); const err = ref(''); const disabled = ref(false); const q = ref(''); const creating = ref(false); const form = ref({ name: '', config: '{\n  "cell_size": 100\n}' }); const nodes = ref<Record<string, string>>({})
async function load() {
  err.value = ''; disabled.value = false
  try {
    const [c, n] = await Promise.all([get('/v1/admin/cluster/worlds'), get('/v1/server/cluster/nodes').catch(() => null)])
    for (const x of n?.nodes ?? []) nodes.value[x.id] = x.name
    const local = new Set<string>(c.local ?? [])
    rows.value = (c.worlds ?? []).map((w: any) => ({ ...w, local: local.has(w.name) }))
  } catch (e: any) { if (e.status === 404 && /worlds are disabled/.test(e.message)) disabled.value = true; else err.value = e.message }
}
onMounted(load)
const shown = computed(() => rows.value.filter((r) => !q.value || r.name.includes(q.value)))
const sim = computed(() => s.node?.roles?.includes('sim'))
function ts(v: any) { return v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 19) : '—' }
async function create() { try { await post('/v1/server/worlds', { name: form.value.name, config: JSON.parse(form.value.config) }); toast.show('world created'); creating.value = false; load() } catch (e: any) { toast.err(e) } }
async function del(w: string) { if (!window.confirm(`Delete world ${w}? Its journal and snapshots are removed.`)) return; try { await api('DELETE', `/v1/server/worlds/${w}`); toast.show('deleted'); load() } catch (e: any) { toast.err(e) } }
const pg1 = usePager(() => shown)
</script>
<template>
  <div class="stats">
    <div class="stat"><div class="k">Server meshing</div><div class="v" :class="{ bad: disabled }">{{ disabled ? 'disabled' : 'enabled' }}<small>{{ disabled ? '[worlds] enabled = false' : 'worlds + cells + handover + failover' }}</small></div></div>
    <div class="stat"><div class="k">This node</div><div class="v">{{ sim ? 'sim' : 'router only' }}<small>{{ sim ? 'hosts cells, adopts orphans' : 'no `sim` role — routes world traffic' }}</small></div></div>
    <div class="stat"><div class="k">Worlds</div><div class="v">{{ rows.length }}<small>{{ rows.filter((r) => r.local).length }} homed on this node</small></div></div>
  </div>
  <div class="card">
    <h2>Worlds <small>({{ shown.length }})</small></h2>
    <p class="err" v-if="err">{{ err }}</p>
    <p class="dim" v-if="disabled">Worlds are switched off on this cluster. Set <code>[worlds] enabled = true</code> and give at least one node the <code>sim</code> role to use persistent worlds.</p>
    <template v-else>
      <div class="tools"><input class="in" v-model="q" placeholder="filter by name" /><button class="btn" @click="load">Refresh</button><button class="btn p" style="margin-left:auto" @click="creating = true">Create world</button></div>
      <table><thead><tr><th>Name</th><th style="width:220px">Home node</th><th style="width:120px">Here</th><th style="width:170px">Created</th><th style="width:80px"></th></tr></thead><tbody>
        <tr v-for="r in pg1.rows" :key="r.name" class="row" @click="router.push(`/world/worlds/${r.name}`)">
          <td class="m">{{ r.name }}</td><td class="dim">{{ nodes[r.node_id] ?? r.node_id ?? 'pending adoption' }}</td><td :class="r.local ? 'ok' : 'dim'">{{ r.local ? 'homed here' : 'remote' }}</td><td class="dim">{{ ts(r.created_at) }}</td>
          <td @click.stop><button class="btn s d" @click="del(r.name)">Delete</button></td>
        </tr>
        <tr v-if="!shown.length"><td colspan="5" class="empty">No Data</td></tr></tbody></table><Pager :p="pg1" />
    </template>
  </div>
  <Drawer v-if="creating" title="Create world" @close="creating = false"><div class="form">
    <div class="f"><label>Name</label><input class="in" v-model="form.name" placeholder="overworld" /></div>
    <div class="f"><label>Config (JSON — cell_size, max_depth, three_d, hibernate_after_ticks, …)</label><textarea class="in" v-model="form.config" style="min-height:220px"></textarea></div>
    <div class="row"><button class="btn" @click="creating = false">Cancel</button><button class="btn p" :disabled="!form.name" @click="create">Create</button></div>
  </div></Drawer>
</template>
