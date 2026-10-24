<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>LiveOps</h1><span class="sub">Everything scheduled against the player calendar — Zanshin stays a separate service under Integrations</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="liveops" />
      <div><router-view /></div>
    </div>
  </div>
</template>
