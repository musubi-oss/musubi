<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute(); const items = catalog.settings
const cur = computed(() => route.path.split('/')[2] ?? 'keys')
</script>
<template>
  <div class="page">
    <div class="head"><h1>Settings</h1><span class="sub">Operators, keys, environments, legal, sanctions, audit</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === 'keys' ? '/settings' : `/settings/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
