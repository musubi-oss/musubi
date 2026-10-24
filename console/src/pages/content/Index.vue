<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Content</h1><span class="sub">Content bundles, config sources, schemas, runtime modules</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="content" />
      <div><router-view /></div>
    </div>
  </div>
</template>
