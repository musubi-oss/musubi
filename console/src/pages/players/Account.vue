<script setup lang="ts">
import { ref, onMounted, watch, computed } from 'vue'
import { useRoute } from 'vue-router'
import { get } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
const route = useRoute()
const id = computed(() => String(route.params.id))
const acct = ref<any>(null); const err = ref(''); const tab = ref('account'); const extra = ref<Record<string, any>>({})
const tabs = ['account', 'wallet & inventory', 'heroes', 'storage', 'social', 'purchases', 'bans & reports', 'security', 'data rights']
async function load() { err.value = ''; try { acct.value = await get(`/v1/admin/accounts/${id.value}`) } catch (e: any) { err.value = e.message } }
async function fetchInto(key: string, path: string) { if (extra.value[key] !== undefined) return; try { extra.value[key] = await get(path) } catch (e: any) { extra.value[key] = { error: e.message } } }
watch(tab, (t) => {
  const a = `/v1/admin/accounts/${id.value}`, s = `/v1/server/accounts/${id.value}`
  if (t === 'security') fetchInto('security', `${a}/security_events?limit=100`)
  if (t === 'account') fetchInto('geo', `${s}/geo`)
}, { immediate: true })
onMounted(load)
</script>
<template>
  <div class="crumb"><router-link to="/players">Accounts</router-link> › <span>{{ acct?.display_name || id }}</span><span class="id">{{ id }}</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="acct">
    <div class="tabs"><a v-for="t in tabs" :key="t" :class="{ on: tab === t }" @click="tab = t">{{ t }}</a></div>
    <template v-if="tab === 'account'">
      <div class="grid">
        <div><Kv k="Account ID" :v="acct.id" mono copy /><Kv k="Display name" :v="acct.display_name" /><Kv k="Status" :v="acct.status ?? (acct.deleted_at ? 'deleted' : 'active')" /></div>
        <div><Kv k="Created" :v="acct.created_at" /><Kv k="Last login" :v="acct.last_login_at" /><Kv k="Locale" :v="acct.locale" /></div>
        <div><Kv k="Country" :v="extra.geo?.country" /><Kv k="City" :v="extra.geo?.city" /><Kv k="Last IP" :v="extra.geo?.ip" mono /></div>
      </div>
      <h3>Identities <small>({{ acct.identities?.length ?? 0 }})</small></h3>
      <table><thead><tr><th>Provider</th><th>Subject</th><th>Linked</th></tr></thead><tbody>
        <tr v-for="i in acct.identities ?? []" :key="i.provider + i.subject"><td>{{ i.provider }}</td><td class="m">{{ i.subject }}</td><td class="dim">{{ i.created_at ?? '—' }}</td></tr>
        <tr v-if="!acct.identities?.length"><td colspan="3" class="empty">No Data</td></tr></tbody></table>
      <h3>Raw</h3><JsonView :value="acct" />
    </template>
    <template v-else-if="tab === 'security'"><JsonView :value="extra.security" /></template>
    <template v-else><div class="empty">{{ tab }} — wired in the next pass</div></template>
  </div>
</template>
