import { useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Player-submitted content (levels, skins, names…) waiting for moderation: approve or reject with a note that the author sees. */
export const Route = createFileRoute('/_app/content/ugc')({
  validateSearch: (s: Record<string, unknown>): { status?: string } => ({ status: typeof s.status === 'string' && s.status !== 'pending' ? s.status : undefined }),
  component: Ugc,
})

const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const TONE: Record<string, 'default' | 'secondary' | 'destructive' | 'outline'> = { pending: 'secondary', approved: 'default', rejected: 'destructive' }

function Ugc() {
  const { status = 'pending' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const q = useSub<Row>(`/v1/admin/ugc?limit=200${status === 'all' ? '' : `&status=${status}`}`)
  const items = listOf(q.data, 'items')
  const [sel, setSel] = useState<Row | null>(null)
  const [decision, setDecision] = useState('approved')
  const [note, setNote] = useState('')
  const { run, pending } = useAct([['sub']], () => setSel(null))
  const open = (r: Row, d: string) => { setSel(r); setDecision(d); setNote(''); }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'UGC' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">UGC moderation</h1><p className="mt-1 text-sm text-muted-foreground">Submissions stay hidden from other players until approved. A rejection note is returned to the author with the status.</p></div>
        <Tabs value={status} onValueChange={(v) => void nav({ search: { status: v === 'pending' ? undefined : v }, replace: true })}><TabsList>{['pending', 'approved', 'rejected', 'all'].map((s) => <TabsTrigger key={s} value={s} className="capitalize">{s}</TabsTrigger>)}</TabsList></Tabs>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Submitted</TableHead><TableHead>Kind</TableHead><TableHead>Title</TableHead><TableHead>Author</TableHead><TableHead>Status</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{q.isPending ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Loading…</TableCell></TableRow> : !items.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">{status === 'pending' ? 'Queue is empty — nothing waiting for review.' : 'Nothing here.'}</TableCell></TableRow> :
            items.map((r) => <TableRow key={String(r.id)}><TableCell className="whitespace-nowrap text-muted-foreground">{ts(r.created_at)}</TableCell><TableCell>{String(r.kind ?? r.type ?? '—')}</TableCell><TableCell className="max-w-[18rem] truncate"><button className="underline" onClick={() => open(r, String(r.status) === 'approved' ? 'rejected' : 'approved')}>{String(r.title ?? r.name ?? r.id)}</button></TableCell><TableCell>{r.owner_id ? <Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(r.owner_id) }}>{String(r.owner_id).slice(0, 8)}…</Link> : '—'}</TableCell><TableCell><Badge variant={TONE[String(r.status)] ?? 'outline'}>{String(r.status ?? '')}</Badge></TableCell>
              <TableCell className="whitespace-nowrap text-right">{String(r.status) !== 'approved' && <Button size="sm" variant="ghost" onClick={() => open(r, 'approved')}>Approve</Button>}{String(r.status) !== 'rejected' && <Button size="sm" variant="ghost" onClick={() => open(r, 'rejected')}>Reject</Button>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={!!sel} onOpenChange={(o) => !o && setSel(null)} title={`Review · ${String(sel?.title ?? sel?.name ?? sel?.id ?? '')}`} submit={decision === 'approved' ? 'Approve' : 'Reject'} destructive={decision === 'rejected'} pending={pending} wide
        onSubmit={() => sel && run(() => raw('POST', `/v1/admin/ugc/${encodeURIComponent(String(sel.id))}`, { decision, note }), decision === 'approved' ? 'Approved' : 'Rejected')}>
        {sel && <JsonView value={Object.fromEntries(Object.entries(sel).filter(([k]) => !['id', 'owner_id', 'status'].includes(k)))} />}
        <Field label="Decision"><Select value={decision} onValueChange={setDecision}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="approved">approved</SelectItem><SelectItem value="rejected">rejected</SelectItem></SelectContent></Select></Field>
        <Field label="Note to author"><Textarea rows={3} value={note} onChange={(e) => setNote(e.target.value)} placeholder={decision === 'rejected' ? 'why it was rejected' : 'optional'} /></Field>
      </FormDialog>
    </div>
  )
}
