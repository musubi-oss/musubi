import { useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Account sanctions as a workflow: proposed → approved/rejected → active → expired/revoked; player appeals with a decision; and the catalogue of reason codes (title, player-facing message, severity, default duration, affected features) moderators pick from. */
export const Route = createFileRoute('/_app/settings/sanctions')({
  validateSearch: (s: Record<string, unknown>): { tab?: string; status?: string; action?: string; appeal?: string } => ({
    tab: ['appeals', 'reasons'].includes(String(s.tab)) ? String(s.tab) : undefined, status: STATUSES.includes(String(s.status)) ? String(s.status) : undefined,
    action: ACTIONS.includes(String(s.action)) ? String(s.action) : undefined, appeal: ['open', 'upheld', 'overturned'].includes(String(s.appeal)) ? String(s.appeal) : undefined,
  }),
  component: Sanctions,
})

const ACTIONS = ['ban', 'mute', 'restrict', 'shadow', 'warn'], STATUSES = ['pending', 'active', 'expired', 'revoked', 'rejected'], SEV = ['low', 'medium', 'high', 'critical']
const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const dur = (r: Row) => (r.expires_at ? `until ${ts(r.expires_at)}` : r.duration_secs ? `${Math.round(Number(r.duration_secs) / 3600)} h` : 'permanent')
const qs = (o: Record<string, unknown>) => { const p = new URLSearchParams(); for (const [k, v] of Object.entries(o)) if (v != null && v !== '') p.set(k, String(v)); return p.toString() }
const ANY = '__any'

function Sanctions() {
  const s = Route.useSearch()
  const tab = s.tab ?? 'sanctions'
  const nav = useNavigate({ from: Route.fullPath })
  const set = (patch: Partial<typeof s>) => void nav({ search: (p) => ({ ...p, ...patch }), replace: true })
  const sq = useSub<Row>(`/v1/admin/sanctions?${qs({ status: s.status, action: s.action, limit: 300 })}`)
  const aq = useSub<Row>(`/v1/admin/appeals?${qs({ status: s.appeal, limit: 200 })}`)
  const rq = useSub<Row>('/v1/admin/bans/reasons')
  const sanctions = listOf(sq.data, 'sanctions'), appeals = listOf(aq.data, 'appeals'), reasons = listOf(rq.data, 'reasons')
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const openAct = (title: string, path: string, ok: string, danger = false) => { setF({ title, path, ok, danger, note: '' }); setDlg('act') }
  const openReason = (r?: Row) => { setF(r ? { ...r, existing: true, features: ((r.features as string[] | undefined) ?? []).join(', '), default_duration_secs: r.default_duration_secs ?? '' } : { code: '', title: '', message: '', severity: 'medium', default_duration_secs: '', features: '' }); setDlg('reason') }
  const err = [sq, aq, rq].find((q) => q.isError)
  const badge = (st: string) => <Badge variant={st === 'active' ? 'destructive' : st === 'pending' || st === 'open' ? 'secondary' : 'outline'}>{st}</Badge>
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Sanctions & appeals' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Sanctions & appeals</h1><p className="mt-1 text-sm text-muted-foreground">Sanctions are proposed from a player's page (or by the chat filter and fraud rules) and need approval here unless the operator has the right to apply directly. Players see the reason code's message; appeals arrive from the client SDK and are decided here.</p></div>
        <Tabs value={tab} onValueChange={(v) => set({ tab: v === 'sanctions' ? undefined : v })}><TabsList className="h-auto flex-wrap">{([['sanctions', `Sanctions (${sanctions.length})`], ['appeals', `Appeals (${appeals.filter((a) => a.status === 'open').length} open)`], ['reasons', `Reason codes (${reasons.length})`]] as [string, string][]).map(([v, l]) => <TabsTrigger key={v} value={v}>{l}</TabsTrigger>)}</TabsList></Tabs>
      </div>
      {err && <Alert variant="destructive"><AlertDescription>{String(err.error)}</AlertDescription></Alert>}
      {tab === 'sanctions' && <Card><CardHeader className="pb-2"><div className="flex flex-wrap gap-2">
        <Select value={s.status ?? ANY} onValueChange={(v) => set({ status: v === ANY ? undefined : v })}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent><SelectItem value={ANY}>any status</SelectItem>{STATUSES.map((x) => <SelectItem key={x} value={x}>{x}</SelectItem>)}</SelectContent></Select>
        <Select value={s.action ?? ANY} onValueChange={(v) => set({ action: v === ANY ? undefined : v })}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent><SelectItem value={ANY}>any action</SelectItem>{ACTIONS.map((x) => <SelectItem key={x} value={x}>{x}</SelectItem>)}</SelectContent></Select>
      </div></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Status</TableHead><TableHead>Action</TableHead><TableHead>Account</TableHead><TableHead>Reason</TableHead><TableHead>Message</TableHead><TableHead>Features</TableHead><TableHead>Duration</TableHead><TableHead>By</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{sq.isPending ? <TableRow><TableCell colSpan={9} className="text-muted-foreground">Loading…</TableCell></TableRow> : !sanctions.length ? <TableRow><TableCell colSpan={9} className="text-muted-foreground">No sanctions match.</TableCell></TableRow> :
            sanctions.map((r) => <TableRow key={String(r.id)}><TableCell>{badge(String(r.status))}</TableCell><TableCell className="text-xs">{String(r.action)}</TableCell><TableCell>{r.account_id ? <Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(r.account_id) }}>{String(r.account_id).slice(0, 8)}…</Link> : '—'}</TableCell><TableCell className="font-mono text-xs">{String(r.reason_code ?? r.reason ?? '—')}</TableCell><TableCell className="max-w-[14rem] truncate text-xs text-muted-foreground">{String(r.message ?? '')}</TableCell><TableCell className="text-xs">{((r.features as string[] | undefined) ?? []).join(' ') || 'account'}</TableCell><TableCell className="whitespace-nowrap text-xs">{dur(r)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{String(r.created_by ?? '')} · {ts(r.created_at)}</TableCell>
              <TableCell className="whitespace-nowrap text-right">{r.status === 'pending' && <><Button size="sm" variant="ghost" onClick={() => openAct('Approve sanction', `/v1/admin/sanctions/${String(r.id)}/approve`, 'Sanction approved and applied')}>Approve</Button><Button size="sm" variant="ghost" onClick={() => openAct('Reject sanction', `/v1/admin/sanctions/${String(r.id)}/reject`, 'Sanction rejected')}>Reject</Button></>}{r.status === 'active' && <Button size="sm" variant="ghost" onClick={() => openAct('Revoke sanction', `/v1/admin/sanctions/${String(r.id)}/revoke`, 'Sanction revoked', true)}>Revoke</Button>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>}
      {tab === 'appeals' && <Card><CardHeader className="pb-2"><Select value={s.appeal ?? ANY} onValueChange={(v) => set({ appeal: v === ANY ? undefined : v })}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent><SelectItem value={ANY}>all</SelectItem>{['open', 'upheld', 'overturned'].map((x) => <SelectItem key={x} value={x}>{x}</SelectItem>)}</SelectContent></Select></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Status</TableHead><TableHead>Account</TableHead><TableHead>Sanction</TableHead><TableHead>Text</TableHead><TableHead>Filed</TableHead><TableHead>Decision</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{aq.isPending ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">Loading…</TableCell></TableRow> : !appeals.length ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">No appeals.</TableCell></TableRow> :
            appeals.map((a) => <TableRow key={String(a.id)}><TableCell>{badge(String(a.status))}</TableCell><TableCell>{a.account_id ? <Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(a.account_id) }}>{String(a.account_id).slice(0, 8)}…</Link> : '—'}</TableCell><TableCell className="font-mono text-xs text-muted-foreground">{String(a.sanction_id ?? '').slice(0, 8)}</TableCell><TableCell className="max-w-[20rem] truncate text-sm">{String(a.text ?? a.body ?? '')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(a.created_at)}</TableCell><TableCell className="max-w-[16rem] truncate text-xs text-muted-foreground">{a.decision_note ? `${String(a.decision_note)} (${String(a.decided_by ?? '?')}, ${ts(a.decided_at)})` : ''}</TableCell>
              <TableCell className="whitespace-nowrap text-right">{a.status === 'open' && <><Button size="sm" variant="ghost" onClick={() => { setF({ title: 'Appeal overturned — lift the sanction', path: `/v1/admin/appeals/${String(a.id)}/decide`, ok: 'Appeal overturned', outcome: 'overturned', note: '' }); setDlg('act') }}>Overturn</Button><Button size="sm" variant="ghost" onClick={() => { setF({ title: 'Appeal upheld — sanction stays', path: `/v1/admin/appeals/${String(a.id)}/decide`, ok: 'Appeal upheld', outcome: 'upheld', note: '', danger: true }); setDlg('act') }}>Uphold</Button></>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>}
      {tab === 'reasons' && <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Reason codes</CardTitle><Button size="sm" onClick={() => openReason()}>Add reason</Button></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Code</TableHead><TableHead>Title</TableHead><TableHead>Severity</TableHead><TableHead>Message to player</TableHead><TableHead>Default duration</TableHead><TableHead>Features</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{rq.isPending ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">Loading…</TableCell></TableRow> : !reasons.length ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">No reason codes — add e.g. <code>cheating</code>, <code>toxicity</code>, <code>rmt</code> so sanctions carry a consistent message.</TableCell></TableRow> :
            reasons.map((r) => <TableRow key={String(r.code)}><TableCell><button className="font-mono text-xs underline" onClick={() => openReason(r)}>{String(r.code)}</button></TableCell><TableCell className="text-sm">{String(r.title ?? '')}</TableCell><TableCell><Badge variant={r.severity === 'critical' || r.severity === 'high' ? 'destructive' : 'outline'}>{String(r.severity ?? '')}</Badge></TableCell><TableCell className="max-w-[16rem] truncate text-xs text-muted-foreground">{String(r.message ?? '')}</TableCell><TableCell className="text-xs">{r.default_duration_secs ? `${Math.round(Number(r.default_duration_secs) / 3600)} h` : 'permanent / none'}</TableCell><TableCell className="text-xs">{((r.features as string[] | undefined) ?? []).join(' ') || 'whole account'}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen(`Delete reason ${String(r.code)}?`, () => run(() => raw('DELETE', `/v1/admin/bans/reasons/${encodeURIComponent(String(r.code))}`), 'Reason deleted'))}>Delete</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>}
      <FormDialog open={dlg === 'act'} onOpenChange={(o) => !o && setDlg('')} title={String(f.title ?? '')} submit="Confirm" destructive={!!f.danger} pending={pending} onSubmit={() => run(() => raw('POST', String(f.path), { ...(f.outcome ? { outcome: f.outcome } : {}), note: f.note || undefined }), String(f.ok))}>
        <Field label="Note (kept in the audit log)"><Textarea rows={3} value={String(f.note ?? '')} onChange={(e) => setF((x) => ({ ...x, note: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'reason'} onOpenChange={(o) => !o && setDlg('')} title={f.existing ? `Reason · ${String(f.code)}` : 'New reason code'} pending={pending} disabled={!f.code || !f.title} onSubmit={() => run(() => raw('PUT', `/v1/admin/bans/reasons/${encodeURIComponent(String(f.code))}`, { title: f.title, message: f.message || undefined, severity: f.severity, default_duration_secs: f.default_duration_secs === '' || f.default_duration_secs == null ? null : Number(f.default_duration_secs), features: String(f.features ?? '').split(/[\s,]+/).filter(Boolean) }), 'Reason saved')}>
        <Field label="Code"><Input className="font-mono" value={String(f.code ?? '')} disabled={!!f.existing} onChange={(e) => setF((x) => ({ ...x, code: e.target.value }))} placeholder="cheating" /></Field>
        <Field label="Title (internal)"><Input value={String(f.title ?? '')} onChange={(e) => setF((x) => ({ ...x, title: e.target.value }))} /></Field>
        <Field label="Message shown to the player"><Textarea rows={3} value={String(f.message ?? '')} onChange={(e) => setF((x) => ({ ...x, message: e.target.value }))} /></Field>
        <Field label="Severity"><Select value={String(f.severity ?? 'medium')} onValueChange={(v) => setF((x) => ({ ...x, severity: v }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent>{SEV.map((x) => <SelectItem key={x} value={x}>{x}</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Default duration (seconds)" hint="empty = permanent / none"><Input type="number" min={0} value={String(f.default_duration_secs ?? '')} onChange={(e) => setF((x) => ({ ...x, default_duration_secs: e.target.value }))} /></Field>
        <Field label="Features" hint="space/comma separated, e.g. chat trading matchmaking; empty = whole account"><Input className="font-mono" value={String(f.features ?? '')} onChange={(e) => setF((x) => ({ ...x, features: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
