<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { sections } from '../router'
import { catalog } from '../catalog'
import Generic from './Generic.vue'
const route = useRoute()
const sec = computed(() => sections.find((s) => s.id === route.params.section) ?? sections[0])
const items = computed(() => catalog[sec.value.id] ?? [])
const groups = computed(() => {
  const g: Record<string, typeof items.value> = {}
  for (const it of items.value) (g[it.group ?? ''] ??= []).push(it)
  return g
})
const cur = computed(() => items.value.find((i) => i.id === (route.params.page || items.value[0]?.id)))
</script>
<template>
  <div class="page">
    <div class="head"><h1>{{ sec.title }}</h1><span class="sub">{{ sec.sub }}</span></div>
    <div class="lay">
      <nav class="side">
        <template v-for="(list, g) in groups" :key="g">
          <div class="grp" v-if="g">{{ g }}</div>
          <router-link v-for="it in list" :key="it.id" :to="`/${sec.id}/${it.id}`" :class="{ on: cur?.id === it.id }">{{ it.title }}</router-link>
        </template>
      </nav>
      <div><Generic v-if="cur" :key="cur.id" :res="cur" /></div>
    </div>
  </div>
</template>
