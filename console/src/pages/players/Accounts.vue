<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { get, qs } from '../../api/client'
import DataTable from '../../components/DataTable.vue'
const router = useRouter()
const q = ref(''); const identity = ref(''); const deleted = ref(false); const rows = ref<any[]>([]); const err = ref(''); const cursor = ref<string | null>(null); const stack = ref<string[]>([])
async function load(c: string | null = null) {
  err.value = ''
  try { const d = await get('/v1/admin/accounts' + qs({ q: q.value, identity: identity.value, include_deleted: deleted.value || undefined, cursor: c, limit: 50 })); rows.value = d.accounts ?? d.items ?? []; cursor.value = d.next_cursor ?? d.cursor ?? null }
  catch (e: any) { err.value = e.message }
}
function next() { if (cursor.value) { stack.value.push(cursor.value); load(cursor.value) } }
function prev() { stack.value.pop(); load(stack.value[stack.value.length - 1] ?? null) }
onMounted(load)
</script>
<template>
  <div class="card">
    <h2>Accounts <small>({{ rows.length }})</small></h2>
    <div class="tools">
      <input class="in w" v-model="q" placeholder="display name, id, e-mail…" @keyup.enter="load()" />
      <input class="in" v-model="identity" placeholder="identity provider:subject" @keyup.enter="load()" />
      <label class="dim"><input type="checkbox" v-model="deleted" /> include deleted</label>
      <button class="btn p" @click="load()">Search</button>
    </div>
    <p class="err" v-if="err">{{ err }}</p>
    <DataTable :cols="[{ key: 'id', mono: true }, { key: 'username' }, { key: 'display_name', label: 'display name' }, { key: 'status', fmt: (r) => r.deleted_at ? 'deleted' : r.disabled_at ? 'disabled' : 'active' }, { key: 'created_at', label: 'created', dim: true }]" :rows="rows" pick @pick="(r) => router.push(`/players/accounts/${r.id}`)" />
    <div class="pager"><button class="btn s" :disabled="!stack.length" @click="prev">Prev</button><button class="btn s" :disabled="!cursor" @click="next">Next</button></div>
  </div>
</template>
