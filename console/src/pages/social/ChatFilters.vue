<script setup lang="ts">
import { usePager } from '../../composables/pager'
import Pager from '../../components/Pager.vue'
import { ref, computed, onMounted } from 'vue'
import { api, get, put } from '../../api/client'
import Drawer from '../../components/Drawer.vue'
import { toast } from '../../stores/toast'
const ask = (m: string) => window.confirm(m)
const filters = ref<any[]>([]); const err = ref(''); const dlg = ref(false); const f = ref<any>(null); const sample = ref('')
async function load() { err.value = ''; try { const r = await get('/v1/admin/chat/filters'); filters.value = Array.isArray(r) ? r : r.filters ?? [] } catch (x: any) { err.value = x.message } }
onMounted(load)
function edit(x: any) { f.value = JSON.parse(JSON.stringify(x ?? { kind: 'regex', pattern: '', action: 'mask', enabled: true, replacement: '***', note: '' })); dlg.value = true }
async function save() { try { await put('/v1/admin/chat/filters', f.value); toast.show('filter saved'); dlg.value = false; load() } catch (x: any) { toast.err(x) } }
async function del(x: any) { try { await api('DELETE', `/v1/admin/chat/filters/${encodeURIComponent(x.id)}`); toast.show('deleted'); load() } catch (e: any) { toast.err(e) } }
function matches(x: any, text: string) { if (!text) return false; try { return x.kind === 'regex' ? new RegExp(x.pattern, 'i').test(text) : text.toLowerCase().split(/\W+/).includes(String(x.pattern).toLowerCase()) } catch { return false } }
const hits = computed(() => filters.value.filter((x) => x.enabled !== false && matches(x, sample.value)))
const preview = computed(() => { let t = sample.value; for (const x of hits.value) { if (x.action === 'block') return '⟂ message blocked'; try { t = x.kind === 'regex' ? t.replace(new RegExp(x.pattern, 'gi'), x.replacement ?? '***') : t.replace(new RegExp(`\\b${x.pattern}\\b`, 'gi'), x.replacement ?? '***') } catch {} } return t })
const pg1 = usePager(() => filters)
</script>
<template>
  <div class="crumb"><span>Chat filters</span><span class="id">{{ filters.filter((x) => x.enabled !== false).length }} active of {{ filters.length }}</span><span style="margin-left:auto"><button class="btn s p" @click="edit(null)">Add filter</button></span></div>
  <p class="err" v-if="err">{{ err }}</p>
  <div class="card">
    <p class="note">Filters run on every chat message in order: <b>mask</b> replaces the match, <b>block</b> drops the message, <b>flag</b> delivers it but opens a chat report for moderators.</p>
    <table><thead><tr><th style="width:80px">Kind</th><th>Pattern</th><th style="width:90px">Action</th><th style="width:120px">Replacement</th><th>Note</th><th style="width:80px">Enabled</th><th style="width:110px"></th></tr></thead><tbody>
      <tr v-for="x in pg1.rows" :key="x.id" :class="{ on: hits.includes(x) }"><td>{{ x.kind }}</td><td class="m row" @click="edit(x)">{{ x.pattern }}</td><td :class="{ bad: x.action === 'block' }">{{ x.action }}</td><td class="m dim">{{ x.action === 'mask' ? (x.replacement ?? '***') : '' }}</td><td class="dim">{{ x.note }}</td><td :class="x.enabled === false ? 'dim' : 'ok'">{{ x.enabled === false ? 'no' : 'yes' }}</td><td><button class="btn s" @click="edit(x)">Edit</button> <button class="btn s" @click="ask('Delete filter?') && del(x)">×</button></td></tr>
      <tr v-if="!filters.length"><td colspan="7" class="empty">No filters — chat is unfiltered</td></tr></tbody></table><Pager :p="pg1" />
    <h3>Try a message <small>(evaluated locally with the same patterns)</small></h3>
    <input class="in" v-model="sample" placeholder="type a message to see which filters fire" />
    <p class="dim" style="margin-top:6px" v-if="sample">{{ hits.length ? `${hits.length} filter(s) fire → ` : 'no filter fires → ' }}<span class="m">{{ preview }}</span></p>
  </div>
  <Drawer v-if="dlg" :title="f.id ? 'Edit filter' : 'New filter'" @close="dlg = false"><div class="form">
    <div class="f"><label>Kind</label><select class="sel" v-model="f.kind"><option value="regex">regex (case-insensitive)</option><option value="word">whole word</option></select></div>
    <div class="f"><label>Pattern</label><input class="in m" v-model="f.pattern" :placeholder="f.kind === 'regex' ? 'b[u\\*]y\\s+gold' : 'badword'" /></div>
    <div class="f" style="display:flex;gap:8px"><div><label>Action</label><select class="sel" v-model="f.action"><option value="mask">mask</option><option value="block">block</option><option value="flag">flag for moderators</option></select></div><div v-if="f.action === 'mask'" style="flex:1"><label>Replacement</label><input class="in m" v-model="f.replacement" /></div></div>
    <div class="f"><label>Note</label><input class="in" v-model="f.note" /></div><div class="f"><label><input type="checkbox" v-model="f.enabled" /> enabled</label></div>
    <div class="row"><button class="btn" @click="dlg = false">Cancel</button><button class="btn p" :disabled="!f.pattern" @click="save">Save</button></div></div></Drawer>
</template>
