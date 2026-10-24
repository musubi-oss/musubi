import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** Economy fraud flags raised by the rules (purchase velocity, refund ratio, trade loops, currency spikes); a blocked flag froze the action until resolved here. */
export const Route = createFileRoute('/_app/economy/fraud')({
  validateSearch: (s: Record<string, unknown>): { show?: string } => ({ show: typeof s.show === 'string' ? s.show : undefined }),
  component: Fraud,
})

const ts = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—')

function Fraud() {
  const { show = 'open' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const q = useSub<Row>('/v1/server/economy/fraud/flags')
  const all = listOf(q.data, 'flags')
  const rows = all.filter((f) => show === 'all' || (show === 'open') === !f.resolved_at)
  const byRule: Record<string, number> = {}; for (const f of rows) byRule[String(f.rule)] = (byRule[String(f.rule)] ?? 0) + 1
  const [sel, setSel] = useState<Row | null>(null)
  const [note, setNote] = useState('')
  const { run, pending } = useAct([['sub']], () => setSel(null))
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Fraud flags' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Fraud flags</h1><p className="mt-1 text-sm text-muted-foreground">{rows.length} {show}{Object.keys(byRule).length ? ' · ' + Object.entries(byRule).map(([r, n]) => `${r} ${n}`).join(' · ') : ''}. Rules raise a flag when a player crosses a limit; <b>blocked</b> means the rule also froze the action until a moderator resolves it.</p></div>
        <Tabs value={show} onValueChange={(v) => void nav({ search: { show: v === 'open' ? undefined : v }, replace: true })}><TabsList className="h-auto max-w-full flex-wrap"><TabsTrigger value="open">Open</TabsTrigger><TabsTrigger value="resolved">Resolved</TabsTrigger><TabsTrigger value="all">All</TabsTrigger></TabsList></Tabs>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <DataTable id="fraud-flags" rows={rows} loading={q.isPending} empty={`No ${show} flags`} rowTo={(r) => `/players/accounts/${r.account_id}?tab=bans`}
          cols={[{ key: 'account_id', mono: true }, { key: 'rule' }, { key: 'count', label: 'count / limit', fmt: (r) => <span className={Number(r.count) > Number(r.limit) ? 'text-destructive' : ''}>{String(r.count)} / {String(r.limit)}</span> }, { key: 'blocked', fmt: (r) => (r.blocked ? <Badge variant="destructive">blocked</Badge> : <span className="text-muted-foreground">no</span>) }, { key: 'created_at', label: 'raised', fmt: (r) => ts(r.created_at), dim: true }, { key: 'resolved_at', label: 'resolved', fmt: (r) => ts(r.resolved_at), dim: true }, { key: 'note', dim: true }]}
          actions={(r) => !r.resolved_at ? <Button size="sm" variant="outline" onClick={() => { setSel(r); setNote('') }}>Resolve</Button> : null} />
      </CardContent></Card>
      <FormDialog open={!!sel} onOpenChange={(o) => !o && setSel(null)} title="Resolve fraud flag" submit="Resolve" pending={pending}
        onSubmit={() => sel && run(() => raw('POST', `/v1/server/economy/fraud/flags/${encodeURIComponent(String(sel.id))}/resolve`, { note }), 'Flag resolved')}>
        {sel && <p className="text-sm text-muted-foreground">{String(sel.rule)} · {String(sel.count)} / {String(sel.limit)}{sel.blocked ? ' · unblocks the player' : ''}</p>}
        <Field label="Note"><Input placeholder="false positive / refunded / banned" value={note} onChange={(e) => setNote(e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
