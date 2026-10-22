<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, get } from '../../api/client'
import Kv from '../../components/Kv.vue'
import JsonView from '../../components/JsonView.vue'
import { toast } from '../../stores/toast'
const route = useRoute(); const router = useRouter()
const name = computed(() => String(route.params.name))
const f = ref<any>(null); const err = ref(''); const audiences = ref<string[]>([]); const dirty = ref(false)
interface Rule { audience: string | null; rollout_pct: number | null; value: string; starts_at: string; ends_at: string }
const rules = ref<Rule[]>([]); const defaultValue = ref('true'); const labels = ref('')
function toIso(v: any) { return v ? new Date(v * (v < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '' }
function fromIso(s: string) { return s ? Math.floor(new Date(s).getTime() / 1000) : null }
async function load() {
  err.value = ''
  try {
    f.value = await get(`/v1/server/flags/${encodeURIComponent(name.value)}`)
    const x = f.value.flag ?? f.value
    f.value = x
    defaultValue.value = JSON.stringify(x.default_value ?? true)
    labels.value = (x.labels ?? []).join(', ')
    rules.value = (x.rules ?? []).map((r: any) => ({ audience: r.audience ?? null, rollout_pct: r.rollout_pct ?? null, value: JSON.stringify(r.value), starts_at: toIso(r.starts_at), ends_at: toIso(r.ends_at) }))
    dirty.value = false
  } catch (e: any) { err.value = e.message }
  try { audiences.value = ((await get('/v1/server/audiences')).audiences ?? []).map((a: any) => a.name) } catch {}
}
onMounted(load)
function add() { rules.value.push({ audience: null, rollout_pct: null, value: 'true', starts_at: '', ends_at: '' }); dirty.value = true }
function move(i: number, d: number) { const j = i + d; if (j < 0 || j >= rules.value.length) return; const [r] = rules.value.splice(i, 1); rules.value.splice(j, 0, r); dirty.value = true }
async function save() {
  try {
    const body = {
      description: f.value.description ?? '', enabled: f.value.enabled, default_value: JSON.parse(defaultValue.value),
      labels: labels.value.split(',').map((s) => s.trim()).filter(Boolean),
      rules: rules.value.map((r) => ({ audience: r.audience || null, rollout_pct: r.rollout_pct === null || r.rollout_pct === ('' as any) ? null : Number(r.rollout_pct), value: JSON.parse(r.value || 'null'), starts_at: fromIso(r.starts_at), ends_at: fromIso(r.ends_at) })),
    }
    await api('PUT', `/v1/server/flags/${encodeURIComponent(name.value)}`, body); toast.show('flag saved'); load()
  } catch (e: any) { toast.err(e) }
}
async function del() { if (!window.confirm(`Delete flag ${name.value}?`)) return; try { await api('DELETE', `/v1/server/flags/${encodeURIComponent(name.value)}`); router.push('/liveops/flags') } catch (e: any) { toast.err(e) } }
</script>
<template>
  <div class="crumb"><router-link to="/liveops/flags">Feature flags</router-link> › <span class="m">{{ name }}</span>
    <span style="margin-left:auto;display:flex;gap:8px"><button class="btn s d" @click="del">Delete</button><button class="btn s p" :disabled="!f" @click="save">Save</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card" v-if="f">
    <div class="grid">
      <div><Kv k="Enabled"><label><input type="checkbox" v-model="f.enabled" @change="dirty = true" /> {{ f.enabled ? 'on — rules evaluate, then default' : 'off — every client gets the default value' }}</label></Kv>
        <Kv k="Default value (JSON)"><input class="in" v-model="defaultValue" @input="dirty = true" /></Kv></div>
      <div><Kv k="Description"><input class="in" v-model="f.description" @input="dirty = true" /></Kv><Kv k="Labels (comma-separated)"><input class="in" v-model="labels" @input="dirty = true" /></Kv></div>
      <div><Kv k="Updated" :v="f.updated_at" /><Kv k="Evaluation" v="first matching rule wins: audience → rollout bucket → time window; otherwise the default" /></div>
    </div>
    <h3>Rules <small>({{ rules.length }}, top first)</small> <button class="btn s p" style="margin-left:8px" @click="add">Add rule</button></h3>
    <table><thead><tr><th style="width:40px">#</th><th style="width:220px">Audience</th><th style="width:110px">Rollout %</th><th>Value (JSON)</th><th style="width:190px">Starts</th><th style="width:190px">Ends</th><th style="width:120px"></th></tr></thead><tbody>
      <tr v-for="(r, i) in rules" :key="i">
        <td class="dim">{{ i + 1 }}</td>
        <td><select class="sel" style="min-width:0;width:100%" v-model="r.audience" @change="dirty = true"><option :value="null">everyone</option><option v-for="a in audiences" :key="a" :value="a">{{ a }}</option><option v-if="r.audience && !audiences.includes(r.audience)" :value="r.audience">{{ r.audience }}</option></select></td>
        <td><input class="in" style="min-width:0;width:100%" type="number" min="0" max="100" v-model.number="r.rollout_pct" placeholder="100" @input="dirty = true" /></td>
        <td><input class="in m" style="min-width:0;width:100%" v-model="r.value" @input="dirty = true" /></td>
        <td><input class="in" style="min-width:0;width:100%" type="datetime-local" v-model="r.starts_at" @input="dirty = true" /></td>
        <td><input class="in" style="min-width:0;width:100%" type="datetime-local" v-model="r.ends_at" @input="dirty = true" /></td>
        <td><span style="display:flex;gap:4px"><button class="btn s" @click="move(i, -1)">↑</button><button class="btn s" @click="move(i, 1)">↓</button><button class="btn s d" @click="rules.splice(i, 1); dirty = true">×</button></span></td>
      </tr>
      <tr v-if="!rules.length"><td colspan="7" class="empty">No rules — the default value applies to everyone</td></tr></tbody></table>
    <p class="note" v-if="dirty" style="margin-top:10px">Unsaved changes</p>
    <h3>Raw</h3><JsonView :value="f" />
  </div>
</template>
