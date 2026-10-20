<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.world
const cur = computed(() => route.path.startsWith('/world/worlds/') ? 'worlds' : String(route.params.page ?? 'worlds'))
</script>
<template>
  <div class="page">
    <div class="head"><h1>World</h1><span class="sub">Persistent worlds, cells and simulators (server meshing)</span></div>
    <div class="lay">
      <nav class="side">
        <router-link v-for="it in items" :key="it.id" :to="it.id === 'worlds' ? '/world' : `/world/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link>
      </nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
