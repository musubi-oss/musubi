<script setup lang="ts">
import { useSession } from '../stores/session'
import { sections } from '../router'
import { useRouter } from 'vue-router'
const s = useSession(); const router = useRouter()
function logout() { s.logout(); router.push('/login') }
</script>
<template>
  <div class="top">
    <div class="brand">
      <div class="logo"><i></i>musubi</div>
      <span class="proj">{{ s.node?.project || 'default' }}</span>
      <span class="badge">{{ s.node?.environment || 'env' }}</span>
      <span class="env"><span class="dot" :class="{ off: !s.authed }"></span>{{ s.node?.name || 'node' }}</span>
      <div class="me">{{ s.node?.region || '' }} <b>{{ s.node?.roles?.join(' · ') }}</b> · <a href="#" @click.prevent="logout">sign out</a></div>
    </div>
    <div class="nav">
      <router-link v-for="x in sections" :key="x.id" :to="'/' + x.id">{{ x.title }}</router-link>
    </div>
  </div>
  <slot />
</template>
