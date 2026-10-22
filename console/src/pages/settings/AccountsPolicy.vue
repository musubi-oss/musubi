<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, onMounted } from 'vue'
import { get, put } from '../../api/client'
import Kv from '../../components/Kv.vue'
import { toast } from '../../stores/toast'
const p = ref<any>(null); const err = ref(''); const fields = ref<{ name: string; kind: string; required: boolean; max_len: number | null; public: boolean }[]>([])
async function load() { err.value = ''; try { p.value = await get('/v1/admin/accounts/policy'); p.value.dormant ||= { action: 'none', days: 0, notice_days: 7 }; fields.value = Object.entries(p.value.fields ?? {}).map(([name, f]: [string, any]) => ({ name, kind: f.kind ?? f.type ?? 'string', required: !!f.required, max_len: f.max_len ?? null, public: !!f.public })) } catch (x: any) { err.value = x.message } }
onMounted(load)
async function save() { try { await put('/v1/admin/accounts/policy', { ...p.value, dormant: { ...p.value.dormant, days: Number(p.value.dormant.days) || 0, notice_days: Number(p.value.dormant.notice_days) || 0 }, fields: Object.fromEntries(fields.value.filter((f) => f.name).map((f) => [f.name, { kind: f.kind, required: f.required, max_len: f.max_len || null, public: f.public }])) }); toast.show('policy saved'); load() } catch (x: any) { toast.err(x) } }
const pg1 = usePager(() => fields)
</script>
<template>
  <div class="crumb"><span>Accounts policy</span><span class="id">dormancy, custom profile fields</span><span style="margin-left:auto"><button class="btn s p" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="p">
    <h3>Dormant accounts</h3>
    <p class="note">Accounts with no login for <i>days</i> get an e-mail notice <i>notice days</i> before the action runs. Erase anonymises personal data (GDPR storage limitation); disable keeps the data but blocks login until the player returns.</p>
    <div class="grid"><div><Kv k="Action"><select class="sel" v-model="p.dormant.action"><option value="none">none — keep forever</option><option value="disable">disable</option><option value="erase">erase personal data</option><option value="delete">delete account</option></select></Kv></div><div><Kv k="After days of inactivity (0 = off)"><input class="in" type="number" min="0" v-model.number="p.dormant.days" /></Kv></div><div><Kv k="Notice days before"><input class="in" type="number" min="0" v-model.number="p.dormant.notice_days" /></Kv></div></div>
    <h3>Custom profile fields <small>(validated on <code>PUT /v1/account</code>)</small> <button class="btn s p" style="margin-left:8px" @click="fields.push({ name: '', kind: 'string', required: false, max_len: 64, public: false })">Add field</button></h3>
    <table><thead><tr><th>Name</th><th style="width:120px">Type</th><th style="width:110px">Max length</th><th style="width:90px">Required</th><th style="width:140px">Visible to others</th><th style="width:60px"></th></tr></thead><tbody>
      <tr v-for="(f, i) in pg1.rows" :key="i"><td><input class="in m" v-model="f.name" placeholder="clan_tag" /></td><td><select class="sel" v-model="f.kind"><option>string</option><option>int</option><option>bool</option><option>url</option><option>enum</option></select></td><td><input class="in" type="number" min="0" v-model.number="f.max_len" :disabled="f.kind !== 'string' && f.kind !== 'url'" /></td><td><input type="checkbox" v-model="f.required" /></td><td><input type="checkbox" v-model="f.public" /></td><td><button class="btn s" @click="fields.splice(i, 1)">×</button></td></tr>
      <tr v-if="!fields.length"><td colspan="6" class="empty">No custom fields — profiles have username, display name, avatar, language, timezone, metadata</td></tr></tbody></table><Pager :p="pg1" />
  </div>
</template>
