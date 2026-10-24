<script setup lang="ts">
// One-line "what is this page" under the section header; text lives in about.ts.
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { about } from '../about'
const index: Record<string, string> = { players: 'accounts', servers: 'fleets', liveops: 'timeline', world: 'worlds', cluster: 'nodes', social: 'chat_filters', economy: 'currencies', content: 'bundles', analytics: 'overview', settings: 'keys' }
const route = useRoute()
const text = computed(() => { const [, s, p] = route.path.split('/'); return about[`${s}/${p || index[s] || ''}`] ?? '' })
</script>
<template><p class="about" v-if="text">{{ text }}</p></template>
