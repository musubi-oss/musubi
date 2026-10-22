<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Social</h1><span class="sub">Matchmaking, matches, chat moderation, stats and ratings</span></div>
    <div class="lay">
      <SideNav section="social" />
      <div><router-view /></div>
    </div>
  </div>
</template>
