import { createRouter, createWebHistory } from 'vue-router'
import { useSession } from './stores/session'

export const sections = [
  { id: 'players', title: 'Players', sub: 'View, search and edit player accounts' },
  { id: 'servers', title: 'Game Servers', sub: 'Fleets, capacity, builds and the UDP edge' },
  { id: 'liveops', title: 'LiveOps', sub: 'Everything scheduled against the player calendar' },
  { id: 'economy', title: 'Economy', sub: 'Currencies, items, rewards, store, progression' },
  { id: 'content', title: 'Content', sub: 'Content bundles, config sources, schemas' },
  { id: 'social', title: 'Social', sub: 'Groups, chat, leaderboards, matchmaking' },
  { id: 'world', title: 'World', sub: 'Persistent worlds, cells and simulators' },
  { id: 'cluster', title: 'Cluster', sub: 'Nodes, mesh, telemetry, storage' },
  { id: 'settings', title: 'Settings', sub: 'Operators, keys, projects, audit' },
]

export const router = createRouter({
  history: createWebHistory('/console/'),
  routes: [
    { path: '/login', component: () => import('./pages/Login.vue'), meta: { public: true } },
    { path: '/', redirect: '/players' },
    { path: '/players', component: () => import('./pages/players/Index.vue'), children: [
      { path: '', component: () => import('./pages/players/Accounts.vue') },
      { path: 'accounts/:id', component: () => import('./pages/players/Account.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue') },
    ]},
    { path: '/:section/:page?', component: () => import('./pages/Section.vue') },
  ],
})

router.beforeEach(async (to) => {
  const s = useSession()
  if (!s.ready) await s.probe()
  if (to.meta.public) return true
  if (!s.authed) return { path: '/login', query: { to: to.fullPath } }
  return true
})
