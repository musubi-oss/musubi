<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Players</h1><span class="sub">View, search and edit player accounts</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="players" />
      <div><router-view /></div>
    </div>
  </div>
</template>
