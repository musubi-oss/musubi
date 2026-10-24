<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useSession } from '../stores/session'
import { auth } from '../api/client'
const s = useSession(); const router = useRouter(); const route = useRoute()
const key = ref(''); const base = ref(auth.base); const busy = ref(false); const err = ref('')
const email = ref(localStorage.getItem('musubi.console.email') ?? ''); const password = ref('')
const mode = ref<'password' | 'key'>(localStorage.getItem('musubi.console.login') === 'key' ? 'key' : 'password')
const advanced = ref(!!auth.base)
function done(ok: boolean) {
  if (ok) { localStorage.setItem('musubi.console.login', mode.value); router.push(String(route.query.to || '/players')) }
  else err.value = s.error || 'could not sign in'
  busy.value = false
}
async function go() {
  busy.value = true; err.value = ''
  done(await s.login(key.value, base.value))
}
async function goPassword() {
  if (!email.value || !password.value) return
  busy.value = true; err.value = ''
  localStorage.setItem('musubi.console.email', email.value.trim())
  done(await s.loginPassword(email.value.trim(), password.value, base.value))
}
function sso() {
  const ret = location.origin + '/console/login'
  location.href = (base.value || '') + '/v1/admin/sso/login?return_to=' + encodeURIComponent(ret)
}
onMounted(async () => {
  const m = location.hash.match(/key=([^&]+)/)
  if (m) { key.value = decodeURIComponent(m[1]); history.replaceState(null, '', location.pathname); await go() }
})
</script>
<template>
  <div class="login"><div class="card">
    <div class="logo" style="margin-bottom:14px"><i></i>musubi console</div>
    <div class="tabs" style="margin-bottom:14px"><a :class="{ on: mode === 'password' }" @click="mode = 'password'">E-mail &amp; password</a><a :class="{ on: mode === 'key' }" @click="mode = 'key'">Operator key</a></div>
    <form class="form" v-if="mode === 'password'" @submit.prevent="goPassword">
      <div class="f"><label>E-mail</label><input class="in" v-model="email" type="email" autocomplete="username" autofocus placeholder="you@studio.example" /></div>
      <div class="f"><label>Password</label><input class="in" v-model="password" type="password" autocomplete="current-password" /></div>
      <div class="f" v-if="advanced"><label>API base (empty = this host)</label><input class="in" v-model="base" placeholder="https://api.example.com" /></div>
      <p class="err" v-if="err">{{ err }}</p>
      <div class="row"><a class="lnk" @click="advanced = !advanced">{{ advanced ? 'hide' : 'API base…' }}</a><button type="button" class="btn" @click="sso">Sign in with SSO</button><button type="submit" class="btn p" :disabled="busy || !email || !password">Sign in</button></div>
      <p class="note">No account yet? An admin adds you under Settings → Operators (or sends an invite).</p>
    </form>
    <form class="form" v-else @submit.prevent="go">
      <div class="f"><label>Operator key</label><input class="in" v-model="key" type="password" autofocus placeholder="admin / support / liveops / ops key" /></div>
      <div class="f"><label>API base (empty = this host)</label><input class="in" v-model="base" placeholder="https://api.example.com" /></div>
      <p class="err" v-if="err">{{ err }}</p>
      <div class="row"><button type="button" class="btn" @click="sso">Sign in with SSO</button><button type="submit" class="btn p" :disabled="busy || !key">Sign in</button></div>
    </form>
  </div></div>
</template>
