<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>World</h1><span class="sub">Persistent worlds, cells and simulators (server meshing)</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="world" />
      <div><router-view /></div>
    </div>
  </div>
</template>
