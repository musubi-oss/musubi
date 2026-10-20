<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.economy
const map: Record<string, string> = { items: 'items', 'reward_tables': 'reward_tables', store: 'store', seasons: 'seasons', progression: 'progression' }
const cur = computed(() => { const seg = route.path.split('/')[2] ?? 'currencies'; return map[seg] ?? seg })
</script>
<template>
  <div class="page">
    <div class="head"><h1>Economy</h1><span class="sub">Currencies, items, rewards, store, progression</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === 'currencies' ? '/economy' : `/economy/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
