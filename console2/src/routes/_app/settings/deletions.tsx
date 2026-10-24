import { useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Scheduled account deletions (GDPR/CCPA requests, dormant sweeps, operator-initiated): what is pending inside its grace period, what was executed, lift a pending one, bulk-import requests, run the sweep. */
export const Route = createFileRoute('/_app/settings/deletions')({
  validateSearch: (s: Record<string, unknown>): { status?: string } => ({ status: ['pending', 'done', 'lifted'].includes(String(s.status)) ? String(s.status) : undefined }),
  component: Deletions,
})

const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const stateOf = (d: Row) => (d.lifted_at ? 'lifted' : d.executed_at || d.done ? 'done' : 'pending')

function Deletions() {
  const { status } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const q = useSub<Row>('/v1/admin/deletions?limit=500')
  const all = listOf(q.data, 'deletions')
  const rows = status ? all.filter((d) => stateOf(d) === status) : all
  const [dlg, setDlg] = useState('')
  const [text, setText] = useState('')
  const [out, setOut] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const entries = text.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => { const [account_id, reason] = l.split(/[,;\t]/).map((s) => s.trim()); return { account_id: account_id ?? '', reason: reason || undefined } })
  const bad = entries.filter((e) => !/^[0-9a-f-]{32,36}$/i.test(e.account_id)).length
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Deletions' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Account deletions</h1><p className="mt-1 text-sm text-muted-foreground">A deletion request puts the account into a grace period (from the accounts policy) during which it is locked but recoverable; the sweep then erases it. Lift a pending request if the player changes their mind.</p></div>
        <Button size="sm" variant="outline" onClick={() => { setText(''); setOut(null); setDlg('import') }}>Import requests</Button>
        <Button size="sm" variant="destructive" disabled={keep.pending} onClick={() => confirmThen('Execute due deletions now? Accounts past their grace period are erased permanently.', () => keep.run(async () => setOut(await raw('POST', '/v1/admin/deletions/sweep')), 'Sweep finished'))}>Run sweep</Button>
      </div>
      <Tabs value={status ?? 'all'} onValueChange={(v) => void nav({ search: { status: v === 'all' ? undefined : v }, replace: true })}><TabsList>{(['all', 'pending', 'done', 'lifted'] as const).map((v) => <TabsTrigger key={v} value={v}>{v} ({v === 'all' ? all.length : all.filter((d) => stateOf(d) === v).length})</TabsTrigger>)}</TabsList></Tabs>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {out != null && dlg !== 'import' && <Card><CardContent className="p-3"><div className="mb-1 text-xs font-medium">Sweep result</div><JsonView value={out} /></CardContent></Card>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Account</TableHead><TableHead>Reason</TableHead><TableHead>Requested</TableHead><TableHead>Due</TableHead><TableHead>State</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{q.isPending ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Loading…</TableCell></TableRow> : !rows.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Nothing here.</TableCell></TableRow> :
            rows.map((d) => { const st = stateOf(d); return <TableRow key={String(d.id)}><TableCell>{d.account_id ? <Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(d.account_id) }}>{String(d.account_id).slice(0, 8)}…</Link> : '—'}</TableCell><TableCell className="max-w-[18rem] truncate text-sm">{String(d.reason ?? d.source ?? '')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(d.requested_at ?? d.created_at)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(d.due_at ?? d.execute_at ?? d.scheduled_for)}</TableCell><TableCell><Badge variant={st === 'pending' ? 'secondary' : st === 'done' ? 'destructive' : 'outline'}>{st}</Badge></TableCell><TableCell className="text-right">{st === 'pending' && <Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => confirmThen('Lift this deletion? The account is unlocked and kept.', () => keep.run(() => raw('POST', `/v1/admin/deletions/${encodeURIComponent(String(d.id))}/lift`), 'Deletion lifted'))}>Lift</Button>}</TableCell></TableRow> })}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg === 'import'} onOpenChange={(o) => !o && setDlg('')} title="Import deletion requests" submit={`Schedule ${entries.length}`} destructive pending={pending} disabled={!entries.length || bad > 0} wide onSubmit={() => run(async () => setOut(await raw('POST', '/v1/admin/deletions/import', { entries })), `${entries.length} deletion request(s) scheduled`)}>
        <Field label="One account per line" hint={bad ? `${bad} line(s) are not account ids` : 'account_id[, reason] — e.g. a request export from your support desk'}><Textarea rows={10} className="font-mono text-xs" value={text} onChange={(e) => setText(e.target.value)} placeholder={'0f1e2d3c-…, gdpr request #4411\n9a8b7c6d-…'} /></Field>
      </FormDialog>
    </div>
  )
}
