<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import DataTable from '../../components/DataTable.vue'
import { toast } from '../../stores/toast'
const st = ref<any>(null); const err = ref(''); const ip = ref(''); const secs = ref(300)
async function load() { err.value = ''; try { st.value = await get('/v1/server/edge') } catch (e: any) { err.value = e.status === 404 ? 'UDP edge proxy is not running on this node ([edge] enabled = false)' : e.message } }
async function block() { try { await post('/v1/server/edge/block', { ip: ip.value, secs: secs.value }); toast.show('blocked'); ip.value = ''; load() } catch (e: any) { toast.err(e) } }
async function unblock(a: string) { try { await post('/v1/server/edge/unblock', { ip: a }); toast.show('unblocked'); load() } catch (e: any) { toast.err(e) } }
onMounted(load)
</script>
<template>
  <div class="card">
    <h2>Edge proxy</h2>
    <p class="err" v-if="err">{{ err }}</p>
    <template v-if="st">
      <div class="grid">
        <div><Kv k="Listen" :v="st.listen" mono /><Kv k="Sessions" :v="st.sessions" /><Kv k="Forwarded / dropped" :v="`${st.forwarded} / ${st.dropped}`" /></div>
        <div><Kv k="Guard" :v="st.guard?.enabled === false ? 'disabled' : 'enabled'" /><Kv k="Per source pps / bytes" :v="`${st.guard?.max_pps_per_source} / ${st.guard?.max_bytes_per_source}`" /><Kv k="Opens per source / global pps" :v="`${st.guard?.max_opens_per_source} / ${st.guard?.global_max_pps || '∞'}`" /></div>
        <div><Kv k="Strikes to block / window / block" :v="`${st.guard?.strikes_to_block} / ${st.guard?.strike_window_secs}s / ${st.guard?.block_secs}s`" /><Kv k="Tracked sources" :v="st.tracked_sources" /><Kv k="Allow / deny" :v="`${(st.guard?.allow ?? []).length} / ${(st.guard?.deny ?? []).length}`" /></div>
      </div>
      <h3>Verdicts</h3>
      <div class="stats"><div class="stat" v-for="(n, k) in st.verdicts ?? {}" :key="String(k)"><div class="k">{{ k }}</div><div class="v">{{ n }}</div></div></div>
      <h3>Blocklist <small>({{ (st.blocklist ?? []).length }})</small></h3>
      <div class="tools"><input class="in" v-model="ip" placeholder="ip to block" /><input class="in" type="number" v-model.number="secs" style="min-width:110px" /><button class="btn p" :disabled="!ip" @click="block">Block</button></div>
      <DataTable :cols="[{ key: 'ip', mono: true }, { key: 'remaining_secs', label: 'remaining (s)' }, { key: 'strikes' }, { key: 'blocks' }, { key: 'reason', dim: true }]" :rows="st.blocklist ?? []">
        <template #actions="{ row }"><button class="btn s" @click="unblock(row.ip)">Unblock</button></template>
      </DataTable>
      <h3>Allow / deny lists</h3><JsonView :value="{ allow: st.guard?.allow ?? [], deny: st.guard?.deny ?? [] }" />
    </template>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
