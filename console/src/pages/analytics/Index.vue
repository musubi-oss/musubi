<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Analytics</h1><span class="sub">Active users, retention, revenue, events, crashes, reports</span></div>
    <div class="lay">
      <SideNav section="analytics" />
      <div><router-view /></div>
    </div>
  </div>
</template>
