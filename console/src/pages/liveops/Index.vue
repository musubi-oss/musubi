<script setup lang="ts">
import { useRoute } from 'vue-router'
import { computed } from 'vue'
import { catalog } from '../../catalog'
const route = useRoute()
const items = catalog.liveops
const groups = computed(() => { const g: Record<string, typeof items> = {}; for (const it of items) (g[it.group ?? ''] ??= []).push(it); return g })
const cur = computed(() => String(route.params.page ?? 'timeline'))
</script>
<template>
  <div class="page">
    <div class="head"><h1>LiveOps</h1><span class="sub">Everything scheduled against the player calendar — Zanshin stays a separate service under Integrations</span></div>
    <div class="lay">
      <nav class="side">
        <template v-for="(list, g) in groups" :key="g">
          <div class="grp" v-if="g">{{ g }}</div>
          <router-link v-for="it in list" :key="it.id" :to="it.id === 'timeline' ? '/liveops' : `/liveops/${it.id}`" :class="{ on: cur === it.id }">{{ it.title }}</router-link>
        </template>
      </nav>
      <div><router-view /></div>
    </div>
  </div>
</template>
