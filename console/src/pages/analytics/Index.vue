<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Analytics</h1><span class="sub">Active users, retention, revenue, events, crashes, reports</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="analytics" />
      <div><router-view /></div>
    </div>
  </div>
</template>
