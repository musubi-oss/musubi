<script setup lang="ts">
// Side panel. Esc closes it (innermost first), width grows with `wide`; body scroll is locked while open.
import { onMounted, onBeforeUnmount } from 'vue'
defineProps<{ title: string; wide?: boolean }>(); const emit = defineEmits<{ close: [] }>()
const stack: (() => void)[] = ((window as any).__drawers ??= [])
const me = () => emit('close')
function key(e: KeyboardEvent) { if (e.key === 'Escape' && stack[stack.length - 1] === me && !(e.target as HTMLElement)?.closest?.('select[open]')) { e.stopPropagation(); me() } }
onMounted(() => { stack.push(me); window.addEventListener('keydown', key); document.body.style.overflow = 'hidden' })
onBeforeUnmount(() => { stack.splice(stack.indexOf(me), 1); window.removeEventListener('keydown', key); if (!stack.length) document.body.style.overflow = '' })
</script>
<template>
  <div class="drawer-bg" @click="emit('close')"></div>
  <div class="drawer" :class="{ wide }" role="dialog" :aria-label="title"><div class="dh"><h2>{{ title }}</h2><button class="x" title="close (Esc)" @click="emit('close')">×</button></div><slot /></div>
</template>
