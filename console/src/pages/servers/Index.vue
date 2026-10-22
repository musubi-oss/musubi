<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Game Servers</h1><span class="sub">Fleets, capacity, builds and the UDP edge</span></div>
    <div class="lay">
      <SideNav section="servers" />
      <div><router-view /></div>
    </div>
  </div>
</template>
