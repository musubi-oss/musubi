<script setup lang="ts">
defineProps<{ title: string }>(); const emit = defineEmits<{ close: [] }>()
</script>
<template>
  <div class="drawer-bg" @click="emit('close')"></div>
  <div class="drawer"><h2>{{ title }}</h2><slot /></div>
</template>
