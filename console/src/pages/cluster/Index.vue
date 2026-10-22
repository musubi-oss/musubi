<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.cluster
const cur = computed(() => String(route.params.page ?? 'nodes'))
</script>
<template>
  <div class="page">
    <div class="head"><h1>Cluster</h1><span class="sub">Nodes, Raft, bus, placement, mesh, telemetry</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === 'nodes' ? '/cluster' : `/cluster/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
