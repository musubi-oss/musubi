<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.players
const groups = computed(() => { const g: Record<string, typeof items> = {}; for (const it of items) (g[it.group ?? ''] ??= []).push(it); return g })
const cur = computed(() => route.path.startsWith('/players/accounts') ? 'accounts' : String(route.params.page ?? 'accounts'))
</script>
<template>
  <div class="page">
    <div class="head"><h1>Players</h1><span class="sub">View, search and edit player accounts</span></div>
    <div class="lay">
      <nav class="side">
        <template v-for="(list, g) in groups" :key="g">
          <div class="grp" v-if="g">{{ g }}</div>
          <router-link v-for="it in list" :key="it.id" :to="it.id === 'accounts' ? '/players' : `/players/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link>
        </template>
      </nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
