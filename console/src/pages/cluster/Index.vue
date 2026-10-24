<script setup lang="ts">
import SideNav from '../../components/SideNav.vue'
import PageIntro from '../../components/PageIntro.vue'
</script>
<template>
  <div class="page">
    <div class="head"><h1>Cluster</h1><span class="sub">Nodes, Raft, bus, placement, mesh, logs</span></div>
    <PageIntro />
    <div class="lay">
      <SideNav section="cluster" />
      <div><router-view /></div>
    </div>
  </div>
</template>
