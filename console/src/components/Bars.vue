<script setup lang="ts">
// Tiny inline bar chart: values left to right, hover for the label; no library, no colour noise.
defineProps<{ values: { label: string; v: number }[]; height?: number }>()
</script>
<template>
  <div class="bars" :style="{ height: (height ?? 72) + 'px' }"><div v-for="(b, i) in values" :key="i" class="b" :title="`${b.label}: ${b.v}`" :style="{ height: Math.max(2, (b.v / Math.max(1, ...values.map((x) => x.v))) * 100) + '%' }"></div></div>
</template>
<style scoped>
.bars{display:flex;align-items:flex-end;gap:2px;width:100%}
.b{flex:1;background:var(--line2);border-radius:2px 2px 0 0;min-width:2px}.b:hover{background:var(--blue2)}
</style>
