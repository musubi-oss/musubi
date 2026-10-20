import { createRouter, createWebHistory } from 'vue-router'
import { useSession } from './stores/session'
import { catalog } from './catalog'

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
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.players.find((x) => x.id === r.params.page) ?? catalog.players[0] }) },
    ]},
    { path: '/servers', component: () => import('./pages/servers/Index.vue'), children: [
      { path: '', component: () => import('./pages/Generic.vue'), props: () => ({ res: catalog.servers[0] }) },
      { path: 'fleets/:id', component: () => import('./pages/servers/Fleet.vue') },
      { path: 'servers/:id', component: () => import('./pages/servers/GameServer.vue') },
      { path: 'edge', component: () => import('./pages/servers/Edge.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.servers.find((x) => x.id === r.params.page) ?? catalog.servers[0] }) },
    ]},
    { path: '/liveops', component: () => import('./pages/liveops/Index.vue'), children: [
      { path: '', component: () => import('./pages/liveops/Timeline.vue') },
      { path: 'flags/:name', component: () => import('./pages/liveops/Flag.vue') },
      { path: 'experiments/:id', component: () => import('./pages/liveops/Experiment.vue') },
      { path: 'configs/:name', component: () => import('./pages/liveops/Config.vue') },
      { path: 'audiences/:name', component: () => import('./pages/liveops/Audience.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.liveops.find((x) => x.id === r.params.page) ?? catalog.liveops[1] }) },
    ]},
    { path: '/world', component: () => import('./pages/world/Index.vue'), children: [
      { path: '', component: () => import('./pages/world/Worlds.vue') },
      { path: 'worlds/:w', component: () => import('./pages/world/World.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.world.find((x) => x.id === r.params.page) ?? catalog.world[0] }) },
    ]},
    { path: '/cluster', component: () => import('./pages/cluster/Index.vue'), children: [
      { path: '', component: () => import('./pages/cluster/Overview.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.cluster.find((x) => x.id === r.params.page) ?? catalog.cluster[0] }) },
    ]},
    { path: '/economy', component: () => import('./pages/economy/Index.vue'), children: [
      { path: '', component: () => import('./pages/Generic.vue'), props: () => ({ res: catalog.economy[0] }) },
      { path: 'items/:id', component: () => import('./pages/economy/Item.vue') },
      { path: 'reward_tables/:id', component: () => import('./pages/economy/RewardTable.vue') },
      { path: 'store/:id', component: () => import('./pages/economy/StoreItem.vue') },
      { path: 'seasons/:id', component: () => import('./pages/economy/Season.vue') },
      { path: 'progression/:id', component: () => import('./pages/economy/Track.vue') },
      { path: ':page', component: () => import('./pages/Generic.vue'), props: (r) => ({ res: catalog.economy.find((x) => x.id === r.params.page) ?? catalog.economy[0] }) },
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
