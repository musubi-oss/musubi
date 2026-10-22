<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.analytics
const cur = computed(() => String(route.params.page ?? route.path.split('/')[2] ?? 'overview'))
</script>
<template>
  <div class="page">
    <div class="head"><h1>Analytics</h1><span class="sub">Active users, retention, revenue, events, crashes, reports</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === 'overview' ? '/analytics' : it.link ? it.link({}) ?? `/analytics/${it.id}` : `/analytics/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
