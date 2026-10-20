<script setup lang="ts">
defineProps<{ k: string; v?: unknown; mono?: boolean; copy?: boolean }>()
function cp(v: unknown) { navigator.clipboard?.writeText(String(v ?? '')) }
function show(v: unknown) { if (v === null || v === undefined || v === '') return '—'; if (typeof v === 'object') return JSON.stringify(v); return String(v) }
</script>
<template>
  <div class="kv"><div class="k">{{ k }}</div>
    <div class="v" :class="{ m: mono }"><slot>{{ show(v) }}</slot><button v-if="copy && v" class="cp" title="copy" @click="cp(v)"></button></div>
  </div>
</template>
