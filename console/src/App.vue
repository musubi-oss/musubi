<script setup lang="ts">
import { useRoute } from 'vue-router'
import Layout from './layout/Layout.vue'
const route = useRoute()
</script>
<template>
  <router-view v-if="route.meta.public" />
  <Layout v-else><router-view /></Layout>
</template>
