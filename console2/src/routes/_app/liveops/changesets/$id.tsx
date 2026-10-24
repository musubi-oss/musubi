import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Economy changeset (`new` creates a draft): staged table changes with diff / validation / preview and the draft → submitted → approved → published flow. */
export const Route = createFileRoute('/_app/liveops/changesets/$id')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: ChangesetPage,
})

const TABLES = ['items', 'currencies', 'store_items', 'reward_tables', 'progression_tracks', 'seasons', 'quests', 'achievements', 'economy_strings', 'energies', 'entitlements', 'iap_products', 'leaderboards', 'rating_queues', 'streaks', 'codes', 'bundles']
const FLOW: Record<string, string> = { draft: 'draft → submit for review', submitted: 'awaiting review → approve / reject', approved: 'approved → publish (or waits for schedule)', scheduled: 'scheduled → publishes automatically', published: 'published — live for all players', rejected: 'rejected → fix and resubmit', cancelled: 'cancelled' }
const tsu = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—')

function ChangesetPage() {
  const { id } = Route.useParams()
  const { tab = 'changes' } = Route.useSearch()
  const isNew = id === 'new'
  const nav = useNavigate({ from: Route.fullPath })
  const C = `/v1/server/economy/changesets/${encodeURIComponent(id)}`
  const q = useQuery({ queryKey: ['changeset', id], enabled: !isNew, queryFn: async ({ signal }) => { const r = await raw<Row>('GET', C, undefined, { signal }); return (r.changeset ?? r) as Row } })
  const diff = useSub(isNew ? null : `${C}/diff`)
  const validation = useSub<Row>(isNew ? null : `${C}/validate`)
  const c = q.data
  const [nf, setNf] = useState({ name: '', note: '', schedule_at: '' })
  const [dlg, setDlg] = useState<'' | 'stage' | 'approve' | 'reject'>('')
  const [form, setForm] = useState<Row>({})
  const [pvTable, setPvTable] = useState(''); const [pvLocale, setPvLocale] = useState('')
  const { run, pending } = useAct([['changeset', id], ['sub']], () => setDlg(''))
  const rawCh = c?.changes ?? c?.staged ?? []
  const changes: Row[] = Array.isArray(rawCh) ? (rawCh as Row[]) : Object.entries(rawCh as Record<string, unknown>).flatMap(([table, rows]) => (Array.isArray(rows) ? rows.map((r: Row) => [r.key, r] as [unknown, Row]) : Object.entries((rows ?? {}) as Record<string, Row>)).map(([k, r]) => ({ table, op: r?.op ?? (r === null ? 'delete' : 'upsert'), key: k, row: r?.row ?? r })))
  const keyOf = (ch: Row) => { const r = (ch.row ?? {}) as Row; return String(ch.key ?? r.id ?? r.code ?? r.name ?? r.key ?? '?') }
  const touched = Object.keys((c?.counts ?? {}) as Record<string, unknown>)
  const pv = pvTable || touched[0] || ''
  const preview = useSub<Row>(!isNew && tab === 'preview' && pv ? `${C}/preview?table=${encodeURIComponent(pv)}${pvLocale ? `&locale=${encodeURIComponent(pvLocale)}` : ''}` : null)
  const state = String(c?.state ?? 'draft')
  const editable = isNew || state === 'draft' || state === 'rejected'
  const problems = (validation.data?.problems ?? []) as Row[]
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Scheduled changesets', to: '/liveops/changesets' }, { title: isNew ? 'new changeset' : String(c?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New changeset' : String(c?.name ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">{c && <Badge variant={state === 'published' ? 'default' : 'secondary'}>{state}</Badge>}{FLOW[state]}{c?.schedule_at ? ` · scheduled ${tsu(c.schedule_at)}` : ''}</div></div>
        {isNew ? <Button size="sm" disabled={!nf.name || pending} onClick={() => run(async () => { const r = await raw<Row>('POST', '/v1/server/economy/changesets', { name: nf.name, note: nf.note, schedule_at: nf.schedule_at ? Math.floor(new Date(nf.schedule_at).getTime() / 1000) : null }); void nav({ to: '/liveops/changesets/$id', params: { id: String(r.id ?? (r.changeset as Row | undefined)?.id ?? '') }, replace: true }) }, 'Draft created')}>Create draft</Button> : c && (
          <div className="flex flex-wrap gap-2">
            {editable && <Button size="sm" variant="outline" onClick={() => { setForm({ table: 'items', op: 'upsert', row: '{\n  \n}' }); setDlg('stage') }}>Stage change</Button>}
            {(state === 'draft' || state === 'rejected') && <Button size="sm" disabled={!changes.length} onClick={() => run(() => raw('POST', `${C}/submit`), 'Submitted for review')}>Submit</Button>}
            {state === 'submitted' && <><Button size="sm" onClick={() => { setForm({ note: '' }); setDlg('approve') }}>Approve</Button><Button size="sm" variant="outline" onClick={() => { setForm({ note: '' }); setDlg('reject') }}>Reject</Button></>}
            {state === 'approved' && <Button size="sm" onClick={() => confirmThen('Publish to the live economy now? A content version is created and all players see it.', () => run(() => raw('POST', `${C}/publish`), 'Published'))}>Publish now</Button>}
            {!['published', 'cancelled'].includes(state) && <Button size="sm" variant="outline" onClick={() => confirmThen('Cancel this changeset?', () => run(() => raw('POST', `${C}/cancel`), 'Cancelled'))}>Cancel</Button>}
            {['draft', 'cancelled', 'rejected'].includes(state) && <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete permanently?', () => run(async () => { await raw('DELETE', C); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'changesets' } }) }, 'Deleted'))}>Delete</Button>}
          </div>
        )}
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {isNew && (
        <Card><CardContent className="pt-6"><KvGrid>
          <Kv k="Name" v={<Input value={nf.name} onChange={(e) => setNf((f) => ({ ...f, name: e.target.value }))} />} />
          <Kv k="Note" v={<Input value={nf.note} placeholder="what and why" onChange={(e) => setNf((f) => ({ ...f, note: e.target.value }))} />} />
          <Kv k="Publish at (optional)" v={<Input type="datetime-local" value={nf.schedule_at} onChange={(e) => setNf((f) => ({ ...f, schedule_at: e.target.value }))} />} />
        </KvGrid></CardContent></Card>
      )}
      {c && (
        <>
          <Card><CardContent className="pt-6"><KvGrid>
            <Kv k="Note" v={c.note} /><Kv k="Created" v={`${tsu(c.created_at)} by ${c.created_by ?? '—'}`} /><Kv k="Schedule" v={c.schedule_at ? tsu(c.schedule_at) : 'publish manually'} />
            <Kv k="Reviewed" v={c.reviewed_by ? `${c.reviewed_by} · ${tsu(c.reviewed_at)} ${c.review_note ? `— ${c.review_note}` : ''}` : '—'} /><Kv k="Published" v={c.published_at ? `${tsu(c.published_at)} → content version ${c.content_version ?? '?'}` : '—'} />
            <Kv k="Validation" v={validation.isPending ? '…' : validation.data?.ok === false ? <span className="text-destructive">{problems.length} problem(s)</span> : 'ok'} />
          </KvGrid></CardContent></Card>
          {problems.length > 0 && <Alert variant="destructive"><AlertTitle>Validation problems</AlertTitle><AlertDescription><ul className="list-disc pl-4">{problems.map((p, i) => <li key={i}>{String(p.table ?? '')} {String(p.key ?? '')}: {String(p.problem ?? p.message ?? JSON.stringify(p))}</li>)}</ul></AlertDescription></Alert>}
          <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'changes' ? undefined : v }, replace: true })}>
            <TabsList><TabsTrigger value="changes">Changes ({changes.length})</TabsTrigger><TabsTrigger value="diff">Diff vs live</TabsTrigger><TabsTrigger value="preview">Preview</TabsTrigger><TabsTrigger value="raw">Raw</TabsTrigger></TabsList>
            <Card className="mt-3"><CardContent className="pt-6">
              <TabsContent value="changes">
                <DataTable id="changeset-changes" rows={changes} empty="Nothing staged yet — stage upserts / deletes against economy tables"
                  cols={[{ key: 'table', mono: true }, { key: 'op' }, { key: 'key', mono: true, fmt: keyOf }, { key: 'row', dim: true, fmt: (ch) => (ch.row ? JSON.stringify(ch.row).slice(0, 160) : '') }]}
                  actions={(ch) => editable ? <Button size="sm" variant="ghost" className="text-destructive" onClick={() => run(() => raw('DELETE', `${C}/changes/${encodeURIComponent(String(ch.table))}/${encodeURIComponent(keyOf(ch))}`), 'Change removed')}>Remove</Button> : null} />
              </TabsContent>
              <TabsContent value="diff"><JsonView value={diff.data} empty="No differences against the live economy" /></TabsContent>
              <TabsContent value="preview">
                <div className="mb-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                  <span>Table as it would be after publishing:</span>
                  <Select value={pv} onValueChange={setPvTable}><SelectTrigger className="h-8 w-44"><SelectValue placeholder="table" /></SelectTrigger><SelectContent>{(touched.length ? touched : TABLES).map((t) => <SelectItem key={t} value={t}>{t}</SelectItem>)}</SelectContent></Select>
                  <Input className="h-8 w-24" value={pvLocale} placeholder="locale" onChange={(e) => setPvLocale(e.target.value)} aria-label="preview locale" />
                  {preview.data && <span>{String(preview.data.count ?? '')} rows</span>}
                </div>
                {!pv ? <p className="text-sm text-muted-foreground">Nothing staged — stage a change to preview a table.</p> : preview.isError ? <Alert variant="destructive"><AlertDescription>{String(preview.error)}</AlertDescription></Alert> : <JsonView value={preview.data?.rows} empty={preview.isPending ? 'Loading…' : 'Empty table'} />}
              </TabsContent>
              <TabsContent value="raw"><JsonView value={c} /></TabsContent>
            </CardContent></Card>
          </Tabs>
        </>
      )}
      <FormDialog open={dlg === 'stage'} onOpenChange={(o) => !o && setDlg('')} title="Stage change" submit="Stage" pending={pending}
        onSubmit={() => { let row: unknown; try { row = JSON.parse(String(form.row || '{}')) } catch { return toast.error('Row is not valid JSON') }; run(() => raw('PUT', `${C}/changes`, { changes: [{ table: form.table, op: form.op, row }] }), 'Change staged') }}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Table"><Select value={String(form.table)} onValueChange={(v) => setForm((f) => ({ ...f, table: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{TABLES.map((t) => <SelectItem key={t} value={t}>{t}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Operation"><Select value={String(form.op)} onValueChange={(v) => setForm((f) => ({ ...f, op: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="upsert">upsert — create or replace</SelectItem><SelectItem value="delete">delete — by key</SelectItem></SelectContent></Select></Field>
        </div>
        <Field label={form.op === 'delete' ? 'Row key (JSON: {"id": …})' : 'Row (JSON, same shape as the table)'}><Textarea className="min-h-40 font-mono text-xs" value={String(form.row ?? '')} onChange={(e) => setForm((f) => ({ ...f, row: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'approve' || dlg === 'reject'} onOpenChange={(o) => !o && setDlg('')} title={dlg === 'approve' ? 'Approve changeset' : 'Reject changeset'} submit={dlg === 'approve' ? 'Approve' : 'Reject'} destructive={dlg === 'reject'} pending={pending}
        onSubmit={() => run(() => raw('POST', `${C}/${dlg}`, { note: form.note }), dlg === 'approve' ? 'Approved' : 'Rejected')}>
        <Field label="Review note"><Input value={String(form.note ?? '')} onChange={(e) => setForm((f) => ({ ...f, note: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
