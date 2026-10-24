import { catalog, type Res } from './catalog'

export interface Section { id: string; title: string; sub: string }
export const sections: Section[] = [
  { id: 'players', title: 'Players', sub: 'View, search and edit player accounts' },
  { id: 'servers', title: 'Game Servers', sub: 'Fleets, capacity, builds and the UDP edge' },
  { id: 'liveops', title: 'LiveOps', sub: 'Everything scheduled against the player calendar' },
  { id: 'economy', title: 'Economy', sub: 'Currencies, items, rewards, store, progression, monetization, codes' },
  { id: 'content', title: 'Content', sub: 'Content bundles, config sources, schemas' },
  { id: 'social', title: 'Social', sub: 'Groups, chat, leaderboards, matchmaking' },
  { id: 'world', title: 'World', sub: 'Persistent worlds, cells and simulators' },
  { id: 'analytics', title: 'Analytics', sub: 'Active users, retention, revenue, events, crashes, reports' },
  { id: 'cluster', title: 'Cluster', sub: 'Nodes, Raft, bus, placement, mesh, logs' },
  { id: 'settings', title: 'Settings', sub: 'Operators, keys, environments, legal, sanctions, audit' },
]

export const sectionById = (id: string) => sections.find((s) => s.id === id)
export const resources = (section: string): Res[] => catalog[section] ?? []
export const resource = (section: string, page: string): Res | undefined => resources(section).find((r) => r.id === page)

/** Side-nav groups for a section, in catalog order; ungrouped entries come first. */
export function navGroups(section: string): { title: string; items: Res[] }[] {
  const out: { title: string; items: Res[] }[] = []
  for (const r of resources(section)) {
    const g = (r as Res & { group?: string }).group ?? ''
    let bucket = out.find((b) => b.title === g)
    if (!bucket) { bucket = { title: g, items: [] }; out.push(bucket) }
    bucket.items.push(r)
  }
  return out.sort((a, b) => (a.title === '' ? -1 : b.title === '' ? 1 : 0))
}
