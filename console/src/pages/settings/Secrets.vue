<script setup lang="ts">
// Credentials at rest (webhook secrets, provider client secrets…): which master key seals
// them, how many rows are plaintext or sealed with a key this node no longer has, and the
// re-encryption sweep after a key rotation. Retention and dormant-account sweeps live here too.
import { ref, onMounted } from 'vue'
import { get, post } from '../../api/client'
import Kv from '../../components/Kv.vue'
import DataTable from '../../components/DataTable.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const x = ref<Record<string, any>>({}); const err = ref(''); const busy = ref('')
async function load() { err.value = ''; try { x.value.s = await get('/v1/admin/secrets/status') } catch (e: any) { err.value = e.message } }
onMounted(load)
const ask = (m: string) => window.confirm(m)
async function run(k: string, fn: () => Promise<any>, ok: string) { busy.value = k; try { x.value[k] = await fn(); toast.show(ok); load() } catch (e: any) { toast.err(e) } finally { busy.value = '' } }
const total = (f: string) => (x.value.s?.stores ?? []).reduce((a: number, s: any) => a + (s[f] ?? 0), 0)
</script>
<template>
  <div class="crumb"><span>Secrets &amp; sweeps</span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="stats">
    <div class="stat"><div class="k">Master key</div><div class="v" :class="{ bad: x.s && !x.s.configured }">{{ x.s ? (x.s.configured ? x.s.current_key ?? 'configured' : 'none') : '—' }}<small>{{ (x.s?.keys ?? []).length }} keys loaded</small></div></div>
    <div class="stat"><div class="k">Sealed with current key</div><div class="v">{{ total('sealed_current') }}<small>of {{ total('rows') - total('empty') }} stored credentials</small></div></div>
    <div class="stat"><div class="k">Plaintext</div><div class="v" :class="{ warn: total('plaintext') }">{{ total('plaintext') }}</div></div>
    <div class="stat"><div class="k">Sealed with other / unknown key</div><div class="v" :class="{ warn: total('sealed_other_key'), bad: total('sealed_unknown_key') }">{{ total('sealed_other_key') }} / {{ total('sealed_unknown_key') }}</div></div>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Stores</h3>
    <div class="tools"><button class="btn p" :disabled="busy === 're' || !x.s?.configured" @click="ask('Re-encrypt every credential with the current master key?') && run('re', () => post('/v1/admin/secrets/reencrypt'), 'rewrapped')">{{ busy === 're' ? 'rewrapping…' : 'Re-encrypt with current key' }}</button><span class="dim">plaintext and other-key rows are rewrapped; unknown-key rows cannot be (that key is not in <code>[security] master_keys</code>)</span></div>
    <DataTable :cols="[{ key: 'table', mono: true }, { key: 'column', mono: true }, { key: 'rows' }, { key: 'empty', dim: true }, { key: 'plaintext', fmt: (r) => String(r.plaintext) }, { key: 'sealed_current', label: 'current key' }, { key: 'sealed_other_key', label: 'other key' }, { key: 'sealed_unknown_key', label: 'unknown key' }]" :rows="x.s?.stores ?? []" />
    <template v-if="(x.s?.warnings ?? []).length"><h3>Warnings</h3><p class="warn" v-for="w in x.s.warnings" :key="w" style="margin:4px 0">{{ w }}</p></template>
    <template v-if="(x.s?.config_refs ?? []).length"><h3>Config references</h3><JsonView :value="x.s.config_refs" /></template>
    <template v-if="x.re"><h3>Last re-encryption</h3><JsonView :value="x.re" /></template>
  </div>
  <div class="card">
    <h3 style="margin-top:0">Sweeps</h3>
    <div class="tools">
      <button class="btn" :disabled="busy === 'ret'" @click="ask('Run retention now? Expired history, journals and telemetry are deleted per the configured windows.') && run('ret', () => post('/v1/admin/retention/run'), 'retention ran')">{{ busy === 'ret' ? 'running…' : 'Run retention now' }}</button>
      <button class="btn" :disabled="busy === 'dor'" @click="ask('Sweep dormant accounts now (per the accounts policy)?') && run('dor', () => post('/v1/admin/accounts/dormant/sweep'), 'dormant sweep ran')">{{ busy === 'dor' ? 'sweeping…' : 'Sweep dormant accounts' }}</button>
      <button class="btn" :disabled="busy === 'del'" @click="ask('Re-apply recorded deletions to any data that came back?') && run('del', () => post('/v1/admin/deletions/sweep'), 'deletions re-applied')">{{ busy === 'del' ? 'sweeping…' : 'Re-apply deletions' }}</button>
    </div>
    <p class="note">All three run on their schedules anyway; these buttons run one pass now and show what it did.</p>
    <template v-if="x.ret"><h3>Retention</h3><JsonView :value="x.ret" /></template>
    <template v-if="x.dor"><h3>Dormant accounts</h3><JsonView :value="x.dor" /></template>
    <template v-if="x.del"><h3>Deletions</h3><JsonView :value="x.del" /></template>
  </div>
  <div class="toast" v-if="toast.msg" :class="{ bad: toast.bad }">{{ toast.msg }}</div>
</template>
