<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useSession } from '../stores/session'
const s = useSession(); const router = useRouter(); const route = useRoute()
const key = ref(''); const base = ref(''); const busy = ref(false); const err = ref('')
async function go() {
  busy.value = true; err.value = ''
  if (await s.login(key.value, base.value)) router.push(String(route.query.to || '/players'))
  else err.value = s.error || 'could not sign in'
  busy.value = false
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
    <div class="form">
      <div class="f"><label>Operator key</label><input class="in" v-model="key" type="password" placeholder="admin / support / liveops / ops key" @keyup.enter="go" /></div>
      <div class="f"><label>API base (empty = this host)</label><input class="in" v-model="base" placeholder="https://api.example.com" /></div>
      <p class="err" v-if="err">{{ err }}</p>
      <div class="row"><button class="btn" @click="sso">Sign in with SSO</button><button class="btn p" :disabled="busy || !key" @click="go">Sign in</button></div>
    </div>
  </div></div>
</template>
