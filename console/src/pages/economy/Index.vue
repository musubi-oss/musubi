<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Economy</h1><span class="sub">Currencies, items, rewards, store, progression, monetization, codes</span></div>
    <div class="lay">
      <SideNav section="economy" />
      <div><router-view /></div>
    </div>
  </div>
</template>
