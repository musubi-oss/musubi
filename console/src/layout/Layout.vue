<script setup lang="ts">
import { onMounted, onBeforeUnmount } from 'vue'
import { useSession } from '../stores/session'
import { sections } from '../router'
import { useRouter } from 'vue-router'
import { toast } from '../stores/toast'
const s = useSession(); const router = useRouter()
function logout() { s.logout(); router.push('/login') }
// `/` focuses the first search / filter box on the page, `r` refreshes it, `Esc` blurs an input.
function key(e: KeyboardEvent) {
  const t = e.target as HTMLElement; const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(t?.tagName) || t?.isContentEditable
  if (e.key === 'Escape' && typing) { (t as HTMLElement).blur(); return }
  if (typing || e.metaKey || e.ctrlKey || e.altKey) return
  if (e.key === '/') { const el = document.querySelector<HTMLInputElement>('.page input.in[placeholder*="search" i], .page input.in[placeholder*="filter" i], .page input.in'); if (el) { e.preventDefault(); el.focus(); el.select() } }
  if (e.key === 'r') { const b = [...document.querySelectorAll<HTMLButtonElement>('.page button.btn')].find((x) => /^refresh$/i.test(x.textContent?.trim() ?? '')); if (b && !document.querySelector('.drawer')) { e.preventDefault(); b.click() } }
}
onMounted(() => window.addEventListener('keydown', key)); onBeforeUnmount(() => window.removeEventListener('keydown', key))
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
      <span class="keys" title="keyboard: / search · r refresh · Esc close">/ · r · Esc</span>
    </div>
  </div>
  <slot />
  <div class="toasts"><div v-for="t in toast.items" :key="t.id" class="toast" :class="{ bad: t.bad }" @click="toast.dismiss(t.id)">{{ t.msg }}</div></div>
</template>
