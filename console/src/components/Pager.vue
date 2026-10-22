<script setup lang="ts">
import type { Pager } from '../composables/pager'
defineProps<{ p: Pager }>()
</script>
<template>
  <div class="pager" v-if="p.shown">
    <span>{{ p.from.toLocaleString('en-US') }}–{{ p.to.toLocaleString('en-US') }} of {{ p.total.toLocaleString('en-US') }}</span>
    <select class="sel sz" :value="p.st.size" @change="p.setSize(Number(($event.target as HTMLSelectElement).value))"><option v-for="s in p.SIZES" :key="s" :value="s">{{ s }} / page</option></select>
    <button class="btn s" :disabled="p.st.page === 0" @click="p.first()" title="first">«</button>
    <button class="btn s" :disabled="p.st.page === 0" @click="p.prev()">Prev</button>
    <span class="pg">{{ p.st.page + 1 }} / {{ p.pages }}</span>
    <button class="btn s" :disabled="p.st.page >= p.pages - 1" @click="p.next()">Next</button>
    <button class="btn s" :disabled="p.st.page >= p.pages - 1" @click="p.last()" title="last">»</button>
  </div>
</template>
