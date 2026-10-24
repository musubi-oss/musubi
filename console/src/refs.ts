// Cross-links by field name: any table cell or key/value whose key is listed here renders as a
// link to the object it names, whatever page it appears on (account_id → the player, world → the
// world, …). `row` lets a field pick its route from a sibling (a session's match, a key's operator).
export type Ref = (value: string, row: any) => string | null
const id = (base: string): Ref => (v) => `${base}/${encodeURIComponent(v)}`
export const refs: Record<string, Ref> = {
  account_id: id('/players/accounts'),
  account: id('/players/accounts'),
  owner: id('/players/accounts'),
  actor_account_id: id('/players/accounts'),
  target_account_id: id('/players/accounts'),
  reporter_id: id('/players/accounts'),
  reported_id: id('/players/accounts'),
  world: id('/world/worlds'),
  world_name: id('/world/worlds'),
  entity: (v, r) => (r?.world ? `/world/worlds/${encodeURIComponent(r.world)}/entities/${encodeURIComponent(v)}` : null),
  entity_id: (v, r) => (r?.world ? `/world/worlds/${encodeURIComponent(r.world)}/entities/${encodeURIComponent(v)}` : null),
  node_id: id('/cluster/nodes'),
  node: id('/cluster/nodes'),
  match_id: id('/cluster/matches'),
  fleet: id('/servers/fleets'),
  fleet_id: id('/servers/fleets'),
  server_id: id('/servers/servers'),
  item_id: id('/economy/items'),
  reward_table: id('/economy/reward_tables'),
  reward_table_id: id('/economy/reward_tables'),
  operator_id: id('/settings/operators'),
  key_id: id('/settings/keys'),
  session_id: id('/social/sessions'),
  tournament_id: id('/social/tournaments'),
  experiment_id: id('/liveops/experiments'),
  flag: id('/liveops/flags'),
  audience: id('/liveops/audiences'),
  bundle_id: id('/content/bundles'),
  module: id('/content/modules'),
  webhook_id: id('/liveops/webhooks'),
  schedule_id: id('/content/schedules'),
}
export function refFor(key: string, value: unknown, row: any): string | null {
  if (value === null || value === undefined || value === '' || typeof value === 'object') return null
  const k = key.split('.').pop() ?? key
  const f = refs[k]
  return f ? f(String(value), row) : null
}
