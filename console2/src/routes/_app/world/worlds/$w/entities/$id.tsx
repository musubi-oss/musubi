import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { qs, raw } from '@/lib/api'
import { listOf, useSub } from '@/lib/sub'
import { refFor } from '@/model/refs'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cellKey } from '../index'

const enc = encodeURIComponent

/** One world entity: where it is (cell, position, hosting simulator), who owns it, its components,
 * children, and every handover it went through. Reached from the world's entity table and from
 * any `entity` cross-link. */
export const Route = createFileRoute('/_app/world/worlds/$w/entities/$id')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: EntityPage,
})

function EntityPage() {
  const { w, id } = Route.useParams()
  const { tab = 'components' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const base = `/v1/server/worlds/${enc(w)}/entities/${enc(id)}`
  const entQ = useQuery({ queryKey: ['entity', w, id], queryFn: async ({ signal }) => { const r = await raw<Row>('GET', base, undefined, { signal }); return (r.entity ?? r) as Row } })
  const childrenQ = useSub<Row>(`${base}/children`)
  const hoQ = useSub<Row>(`/v1/admin/worlds/${enc(w)}/handovers` + qs({ entity: id, limit: 200 }))
  const ent = entQ.data
  const children = useMemo(() => { const c = childrenQ.data; return (c?.children ?? c?.entities ?? (Array.isArray(c) ? c : [])) as Row[] }, [childrenQ.data])
  const hos = useMemo(() => (listOf(hoQ.data, 'handovers').length ? listOf(hoQ.data, 'handovers') : listOf(hoQ.data, 'steps')).filter((x) => String(x.entity ?? x.entity_id ?? x.id) === id), [hoQ.data, id])
  const comps = Object.entries((ent?.components ?? {}) as Record<string, unknown>)
  const pos = ent?.pos ? `${Number(ent.pos.x).toFixed(2)}, ${Number(ent.pos.y).toFixed(2)}${ent.pos.z != null ? ', ' + Number(ent.pos.z).toFixed(2) : ''}` : '—'
  const worldTo = `/world/worlds/${enc(w)}`

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'World', to: '/world' }, { title: 'Worlds', to: '/world/worlds' }, { title: w, to: worldTo }, { title: 'entities', to: `${worldTo}?tab=entities` }, { title: '…' + id.slice(-12) }]} />
      <div>
        <h1 className="truncate font-mono text-xl font-semibold">{id}</h1>
        <p className="mt-1 text-xs text-muted-foreground">entity in world <span className="font-mono">{w}</span></p>
      </div>
      {entQ.isError && <Alert variant="destructive"><AlertDescription>{String(entQ.error)}</AlertDescription></Alert>}
      {entQ.isPending && <Skeleton className="h-32 w-full" />}
      {ent && (
        <>
          <Card><CardContent className="pt-6"><KvGrid className="xl:grid-cols-4">
            <Kv k="cell" v={cellKey(ent.cell)} mono />
            <Kv k="position" v={pos} mono copy />
            <Kv k="owner (account)" v={ent.owner ?? 'unowned — any session may drive it'} mono={!!ent.owner} to={ent.owner ? refFor('owner', ent.owner, ent) : null} copy={!!ent.owner} />
            <Kv k="simulator" v={ent.sim ?? ent.host ?? '—'} mono />
            <Kv k="persistence" v={ent.persistent ? 'journaled' : 'volatile'} />
            <Kv k="global" v={ent.global ? 'yes — every subscriber' : 'no — interest-managed'} />
            <Kv k="in transit" v={ent.remote_to ? `→ node ${ent.remote_to}` : ent.transit ? `→ ${cellKey(ent.transit.to ?? ent.transit)}` : 'no'} />
            <Kv k="parent" v={ent.parent ?? '—'} mono={!!ent.parent} to={ent.parent ? `${worldTo}/entities/${enc(String(ent.parent))}` : null} />
          </KvGrid></CardContent></Card>
          <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'components' ? undefined : v }, replace: true })}>
            <TabsList>
              <TabsTrigger value="components">Components ({comps.length})</TabsTrigger>
              <TabsTrigger value="children">Children ({children.length})</TabsTrigger>
              <TabsTrigger value="handovers">Handovers ({hos.length})</TabsTrigger>
              <TabsTrigger value="json">JSON</TabsTrigger>
            </TabsList>
            <Card className="mt-3"><CardContent className="pt-6">
              <TabsContent value="components">
                {!comps.length && <p className="text-sm text-muted-foreground">No components — the entity is a bare position.</p>}
                {comps.map(([k, v]) => <div key={k} className="mb-3"><div className="mb-1 font-mono text-xs text-muted-foreground">{k}</div><JsonView value={v} /></div>)}
              </TabsContent>
              <TabsContent value="children">
                <DataTable id="entity-children" rows={children} loading={childrenQ.isPending} empty="No child entities" rowTo={(r) => `${worldTo}/entities/${enc(String(r.id))}`}
                  cols={[{ key: 'id', mono: true }, { key: 'cell', mono: true, fmt: (r) => cellKey(r.cell) }, { key: 'persistent', fmt: (r) => (r.persistent ? 'journaled' : 'volatile') }]} />
              </TabsContent>
              <TabsContent value="handovers">
                <DataTable id="entity-ho" rows={hos} loading={hoQ.isPending} empty="No handovers recorded for this entity"
                  cols={[{ key: 'ms', label: 'at', dim: true, fmt: (r) => (r.ms ? new Date(Number(r.ms)).toISOString().slice(11, 23) : (r.at ?? '')) }, { key: 'tick', dim: true }, { key: 'event' }, { key: 'path' }, { key: 'node', mono: true, dim: true }, { key: 'detail', dim: true }]} />
              </TabsContent>
              <TabsContent value="json"><JsonView value={ent} /></TabsContent>
            </CardContent></Card>
          </Tabs>
        </>
      )}
    </div>
  )
}
