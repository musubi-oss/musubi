<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute(); const items = catalog.content
const cur = computed(() => route.path.split('/')[2] ?? 'bundles')
</script>
<template>
  <div class="page">
    <div class="head"><h1>Content</h1><span class="sub">Content bundles, config sources, schemas, runtime modules</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === 'bundles' ? '/content' : `/content/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
