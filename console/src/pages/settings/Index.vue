<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Settings</h1><span class="sub">Operators, keys, environments, legal, sanctions, audit</span></div>
    <div class="lay">
      <SideNav section="settings" />
      <div><router-view /></div>
    </div>
  </div>
</template>
