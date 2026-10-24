import { Link, createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { ApiError, raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'

/** UDP edge proxy of this node: sessions, guard limits, verdicts and the blocklist. */
export const Route = createFileRoute('/_app/servers/edge')({ component: Edge })

function Edge() {
  const q = useQuery({ queryKey: ['edge'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/edge', undefined, { signal }), refetchInterval: 3000, retry: false })
  const off = q.error instanceof ApiError && q.error.status === 404
  const st = q.data
  const g = (st?.guard ?? {}) as Row
  const [ip, setIp] = useState(''); const [secs, setSecs] = useState(300)
  const { run } = useAct([['edge']], () => setIp(''))
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Servers', to: '/servers' }, { title: 'Edge proxy' }]} />
      <div><h1 className="text-xl font-semibold">Edge proxy</h1><p className="mt-1 text-xs text-muted-foreground">The node's UDP front door: game traffic is terminated here, rate-guarded per source and forwarded to game servers.</p></div>
      {off && <Alert><AlertTitle>Not configured on this node</AlertTitle><AlertDescription>The UDP edge proxy is not running — set <code>[edge] enabled = true</code> in the node config to terminate game traffic here. <Link to="/$section/$page" params={{ section: 'settings', page: 'config' }} className="underline">node config →</Link></AlertDescription></Alert>}
      {q.isError && !off && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {st && (
        <Card><CardContent className="pt-6">
          <KvGrid>
            <Kv k="Listen" v={st.listen} mono /><Kv k="Sessions" v={st.sessions} /><Kv k="Forwarded / dropped" v={`${st.forwarded ?? 0} / ${st.dropped ?? 0}`} />
            <Kv k="Guard" v={g.enabled === false ? 'disabled' : 'enabled'} /><Kv k="Per source pps / bytes" v={`${g.max_pps_per_source ?? '—'} / ${g.max_bytes_per_source ?? '—'}`} /><Kv k="Opens per source / global pps" v={`${g.max_opens_per_source ?? '—'} / ${g.max_global_pps ?? '—'}`} />
            <Kv k="Strikes to block / window / block" v={`${g.strikes_to_block ?? '—'} / ${g.strike_window_secs ?? '—'}s / ${g.block_secs ?? '—'}s`} /><Kv k="Tracked sources" v={st.tracked_sources} /><Kv k="Allow / deny" v={`${((g.allow ?? []) as unknown[]).length} / ${((g.deny ?? []) as unknown[]).length}`} />
          </KvGrid>
          <SubHead title="Verdicts" />
          <div className="grid gap-3 sm:grid-cols-3 xl:grid-cols-6">{Object.entries((st.verdicts ?? {}) as Record<string, number>).map(([k, n]) => <div key={k} className="rounded-md border p-3"><div className="text-xs text-muted-foreground">{k}</div><div className="text-xl font-semibold tabular-nums">{n}</div></div>)}{!Object.keys((st.verdicts ?? {}) as object).length && <p className="text-sm text-muted-foreground">No verdicts yet</p>}</div>
          <SubHead title="Blocklist" count={((st.blocklist ?? []) as unknown[]).length} />
          <form className="mb-3 flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); if (ip) run(() => raw('POST', '/v1/server/edge/block', { ip, secs }), 'Blocked') }}>
            <Input className="w-56 font-mono" value={ip} placeholder="ip to block" onChange={(e) => setIp(e.target.value)} />
            <Input className="w-28" type="number" min={1} value={secs} onChange={(e) => setSecs(Number(e.target.value))} aria-label="seconds" />
            <Button type="submit" size="sm" disabled={!ip}>Block</Button>
          </form>
          <DataTable id="edge-blocklist" tools={false} rows={(st.blocklist ?? []) as Row[]} empty="Nobody is blocked"
            cols={[{ key: 'ip', mono: true }, { key: 'remaining_secs', label: 'remaining (s)' }, { key: 'strikes' }, { key: 'blocks' }, { key: 'reason', dim: true }]}
            actions={(r) => <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', '/v1/server/edge/unblock', { ip: r.ip }), 'Unblocked')}>Unblock</Button>} />
          <SubHead title="Allow / deny lists" /><JsonView value={{ allow: g.allow ?? [], deny: g.deny ?? [] }} />
        </CardContent></Card>
      )}
    </div>
  )
}
