import { createFileRoute, Link } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** Cron schedules registered by WASM modules (with the problems the runtime found: missing module/function, bad cron), the timer sweep, and remote config sources that pull configs on a schedule. */
export const Route = createFileRoute('/_app/content/schedules')({ component: Schedules })

const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))

function Schedules() {
  const sq = useSub<Row>('/v1/admin/runtime/schedules')
  const schedules = listOf(sq.data, 'schedules')
  const problems = (sq.data?.problems as Row[] | undefined) ?? []
  const cq = useSub<Row>('/v1/server/config_sources')
  const sources = listOf(cq.data, 'sources')
  const { run, pending } = useAct([['sub']])
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'Schedules & timers' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Schedules & timers</h1><p className="mt-1 text-sm text-muted-foreground">Modules declare cron schedules in their manifest; the node runs each on one elected node only. Timers set from module code are swept on an interval — sweep now fires everything already due.</p></div>
        <Button size="sm" variant="outline" disabled={pending} onClick={() => run(() => raw('POST', '/v1/admin/runtime/timers/sweep'), 'Timer sweep triggered')}>Sweep timers</Button>
      </div>
      {sq.isError && <Alert variant="destructive"><AlertDescription>{String(sq.error)}</AlertDescription></Alert>}
      {problems.length > 0 && <Alert variant="destructive"><AlertDescription><div className="mb-1 font-medium">{problems.length} schedule{problems.length > 1 ? 's' : ''} cannot run</div><ul className="list-disc pl-4 text-sm">{problems.map((p, i) => <li key={i}>{p.module ? <Link className="font-mono underline" to="/content/modules/$name" params={{ name: String(p.module) }}>{String(p.module)}</Link> : null}{p.id || p.name ? <span className="font-mono"> {String(p.id ?? p.name)}</span> : null}: {String(p.error ?? p.reason ?? p.message ?? JSON.stringify(p))}</li>)}</ul></AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Id</TableHead><TableHead>Module · function</TableHead><TableHead>Cron</TableHead><TableHead>Last run</TableHead><TableHead>Next</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{sq.isPending ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Loading…</TableCell></TableRow> : !schedules.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">No schedules — none of the loaded modules declares one.</TableCell></TableRow> :
            schedules.map((s) => { const id = String(s.id ?? s.name); return <TableRow key={id}><TableCell className="font-mono text-xs">{id}</TableCell><TableCell>{s.module ? <Link className="font-mono underline" to="/content/modules/$name" params={{ name: String(s.module) }}>{String(s.module)}</Link> : '—'}{s.function ? <span className="text-muted-foreground"> · {String(s.function)}</span> : null}</TableCell><TableCell className="font-mono text-xs">{String(s.cron ?? s.kind ?? '—')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(s.last_run_at ?? s.last_run)}{s.last_error ? <Badge variant="destructive" className="ml-2">error</Badge> : null}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(s.next_run_at ?? s.next_run)}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={pending} onClick={() => run(() => raw('POST', `/v1/admin/runtime/schedules/${encodeURIComponent(id)}/run`), `${id} started`)}>Run now</Button></TableCell></TableRow> })}</TableBody></Table>
        {schedules.some((s) => s.last_error) && <ul className="border-t p-3 text-xs text-destructive">{schedules.filter((s) => s.last_error).map((s) => <li key={String(s.id ?? s.name)}><span className="font-mono">{String(s.id ?? s.name)}</span>: {String(s.last_error)}</li>)}</ul>}
      </CardContent></Card>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Config sources</CardTitle><p className="text-sm text-muted-foreground">Configs attached to a git/HTTP source are re-pulled on the schedule shown; sync pulls immediately, detach keeps the config but stops syncing.</p></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Config</TableHead><TableHead>Source</TableHead><TableHead>Ref</TableHead><TableHead>Last sync</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{cq.isPending ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">Loading…</TableCell></TableRow> : !sources.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">No config is attached to a remote source. Attach one from the config's page under LiveOps → Configs.</TableCell></TableRow> :
            sources.map((s) => <TableRow key={String(s.name)}><TableCell><Link className="font-mono underline" to="/liveops/configs/$name" params={{ name: String(s.name) }}>{String(s.name)}</Link></TableCell><TableCell className="max-w-[20rem] truncate text-xs">{String(s.kind ?? '')} {String(s.url ?? '')}</TableCell><TableCell className="font-mono text-xs">{String(s.ref ?? s.path ?? '—')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(s.last_sync_at ?? s.synced_at)}{s.last_error ? <Badge variant="destructive" className="ml-2">error</Badge> : null}</TableCell>
              <TableCell className="whitespace-nowrap text-right"><Button size="sm" variant="ghost" disabled={pending} onClick={() => run(() => raw('POST', `/v1/server/configs/${encodeURIComponent(String(s.name))}/source/sync`), `${String(s.name)} synced`)}>Sync</Button><Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen(`Detach ${String(s.name)} from its source?`, () => run(() => raw('DELETE', `/v1/server/configs/${encodeURIComponent(String(s.name))}/source`), `${String(s.name)} detached`))}>Detach</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
    </div>
  )
}
