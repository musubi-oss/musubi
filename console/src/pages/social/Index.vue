<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute(); const items = catalog.social
const cur = computed(() => route.path.split('/')[2] ?? items[0].id)
</script>
<template>
  <div class="page">
    <div class="head"><h1>Social</h1><span class="sub">Matchmaking, matches, chat moderation, stats and ratings</span></div>
    <div class="lay">
      <nav class="side"><router-link v-for="it in items" :key="it.id" :to="it.id === items[0].id ? '/social' : `/social/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link></nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
