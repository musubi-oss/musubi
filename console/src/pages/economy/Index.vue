<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.economy
const groups = computed(() => { const g: Record<string, typeof items> = {}; for (const it of items) (g[it.group ?? ''] ??= []).push(it); return g })
const map: Record<string, string> = { items: 'items', 'reward_tables': 'reward_tables', store: 'store', seasons: 'seasons', progression: 'progression' }
const cur = computed(() => { const seg = route.path.split('/')[2] ?? 'currencies'; return map[seg] ?? seg })
</script>
<template>
  <div class="page">
    <div class="head"><h1>Economy</h1><span class="sub">Currencies, items, rewards, store, progression, monetization, codes</span></div>
    <div class="lay">
      <nav class="side"><template v-for="(list, g) in groups" :key="g"><div class="grp" v-if="g">{{ g }}</div><router-link v-for="it in list" :key="it.id" :to="it.id === 'currencies' ? '/economy' : `/economy/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></template></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
