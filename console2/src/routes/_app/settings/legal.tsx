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
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** Terms, privacy policy, EULA… as versioned documents per locale: publishing a new version makes clients re-prompt (mandatory ones block play after the grace period), and every acceptance or refusal is recorded per account. */
export const Route = createFileRoute('/_app/settings/legal')({
  validateSearch: (s: Record<string, unknown>): { key?: string } => ({ key: typeof s.key === 'string' && s.key ? s.key : undefined }),
  component: Legal,
})

const L = '/v1/admin/legal'
const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const ANY = '__any'

function Legal() {
  const { key } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const q = useSub<Row>(`${L}${key ? `?key=${encodeURIComponent(key)}` : ''}`)
  const docs = listOf(q.data, 'documents')
  const keys = [...new Set(docs.map((d) => String(d.key)))]
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const [acc, setAcc] = useState('')
  const cq = useSub<Row>(dlg === 'consents' && f.key ? `${L}/${encodeURIComponent(String(f.key))}/consents?version=${String(f.version)}&limit=200${acc ? `&accepted=${acc}` : ''}` : null)
  const consents = listOf(cq.data, 'consents')
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const openEdit = (r?: Row) => { setF(r ? { key: r.key, version: Number(r.version) + 1, locale: r.locale, title: r.title, url: r.url ?? '', body: '', mandatory: !!r.mandatory, countries: ((r.countries as string[] | undefined) ?? []).join(', '), grace_secs: r.grace_secs ?? 0, existing: true } : { key: '', version: 1, locale: 'en', title: '', url: '', body: '', mandatory: true, countries: '', grace_secs: 0 }); setDlg('edit') }
  const save = () => run(() => raw('PUT', `${L}/${encodeURIComponent(String(f.key))}/${String(f.version)}`, { locale: f.locale, title: f.title, url: f.url || undefined, body: f.body || undefined, mandatory: !!f.mandatory, countries: String(f.countries ?? '').split(',').map((s) => s.trim().toUpperCase()).filter(Boolean), grace_secs: Number(f.grace_secs) || 0 }), `Published ${String(f.key)} v${String(f.version)}`)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Legal documents' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Legal documents</h1><p className="mt-1 text-sm text-muted-foreground">Clients fetch the current version per locale and country at login; a new version of a mandatory document re-prompts players and, after the grace period, blocks play until accepted. Versions are never edited in place — publish the next one.</p></div>
        {keys.length > 1 || key ? <Select value={key ?? ANY} onValueChange={(v) => void nav({ search: { key: v === ANY ? undefined : v }, replace: true })}><SelectTrigger className="w-44"><SelectValue /></SelectTrigger><SelectContent><SelectItem value={ANY}>all documents</SelectItem>{(key && !keys.includes(key) ? [key, ...keys] : keys).map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent></Select> : null}
        <Button size="sm" onClick={() => openEdit()}>New document</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Key</TableHead><TableHead>Version</TableHead><TableHead>Locale</TableHead><TableHead>Title</TableHead><TableHead>Mandatory</TableHead><TableHead>Countries</TableHead><TableHead>Grace</TableHead><TableHead>Published</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{q.isPending ? <TableRow><TableCell colSpan={9} className="text-muted-foreground">Loading…</TableCell></TableRow> : !docs.length ? <TableRow><TableCell colSpan={9} className="text-muted-foreground">No documents yet. Add <code>tos</code> and <code>privacy</code> so clients can prompt for consent.</TableCell></TableRow> :
            docs.map((r) => <TableRow key={`${String(r.key)}-${String(r.version)}-${String(r.locale)}`}><TableCell className="font-mono text-xs">{String(r.key)}</TableCell><TableCell className="text-xs">v{String(r.version)}{r.current ? <Badge className="ml-1" variant="secondary">current</Badge> : null}</TableCell><TableCell className="text-xs">{String(r.locale ?? '')}</TableCell><TableCell className="max-w-[16rem] truncate text-sm">{r.url ? <a className="underline" href={String(r.url)} target="_blank" rel="noreferrer">{String(r.title)}</a> : String(r.title ?? '')}</TableCell><TableCell>{r.mandatory ? <Badge variant="destructive">mandatory</Badge> : <Badge variant="outline">optional</Badge>}</TableCell><TableCell className="text-xs">{((r.countries as string[] | undefined) ?? []).join(' ') || 'everywhere'}</TableCell><TableCell className="text-xs">{Number(r.grace_secs ?? 0) ? `${Math.round(Number(r.grace_secs) / 3600)} h` : 'none'}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(r.created_at ?? r.published_at)}</TableCell>
              <TableCell className="whitespace-nowrap text-right"><Button size="sm" variant="ghost" onClick={() => { setF(r); setAcc(''); setDlg('consents') }}>Consents</Button><Button size="sm" variant="ghost" onClick={() => openEdit(r)}>New version</Button><Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => confirmThen(`Delete all versions of ${String(r.key)}? Recorded consents are kept.`, () => keep.run(() => raw('DELETE', `${L}/${encodeURIComponent(String(r.key))}`), 'Document deleted'))}>Delete</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg === 'edit'} onOpenChange={(o) => !o && setDlg('')} title={f.existing ? `${String(f.key)} · publish v${String(f.version)}` : 'New legal document'} submit="Publish" pending={pending} disabled={!f.key || !f.title || (!f.url && !f.body)} wide onSubmit={save}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Key"><Input className="font-mono" value={String(f.key ?? '')} disabled={!!f.existing} onChange={(e) => setF((x) => ({ ...x, key: e.target.value }))} placeholder="tos" /></Field>
          <Field label="Version"><Input type="number" min={1} value={String(f.version ?? 1)} onChange={(e) => setF((x) => ({ ...x, version: e.target.value }))} /></Field>
          <Field label="Locale"><Input value={String(f.locale ?? 'en')} onChange={(e) => setF((x) => ({ ...x, locale: e.target.value }))} /></Field>
          <Field label="Title"><Input value={String(f.title ?? '')} onChange={(e) => setF((x) => ({ ...x, title: e.target.value }))} /></Field>
          <Field label="URL" hint="link to the hosted document — or paste the text below"><Input value={String(f.url ?? '')} onChange={(e) => setF((x) => ({ ...x, url: e.target.value }))} /></Field>
          <Field label="Countries" hint="ISO codes, comma-separated; empty = everywhere"><Input className="font-mono" value={String(f.countries ?? '')} onChange={(e) => setF((x) => ({ ...x, countries: e.target.value }))} placeholder="DE, FR, KR" /></Field>
          <Field label="Grace period (seconds)" hint="how long players may keep playing before a mandatory version blocks them"><Input type="number" min={0} value={String(f.grace_secs ?? 0)} onChange={(e) => setF((x) => ({ ...x, grace_secs: e.target.value }))} /></Field>
          <label className="flex items-center gap-2 self-end pb-2 text-sm"><Checkbox checked={!!f.mandatory} onCheckedChange={(c) => setF((x) => ({ ...x, mandatory: !!c }))} />Mandatory</label>
        </div>
        <Field label="Body (optional, Markdown)"><Textarea rows={8} value={String(f.body ?? '')} onChange={(e) => setF((x) => ({ ...x, body: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'consents'} onOpenChange={(o) => !o && setDlg('')} title={`Consents · ${String(f.key ?? '')} v${String(f.version ?? '')}`} submit="Close" wide onSubmit={() => setDlg('')}>
        <Select value={acc || ANY} onValueChange={(v) => setAcc(v === ANY ? '' : v)}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent><SelectItem value={ANY}>all</SelectItem><SelectItem value="true">accepted</SelectItem><SelectItem value="false">declined</SelectItem></SelectContent></Select>
        {cq.isError ? <Alert variant="destructive"><AlertDescription>{String(cq.error)}</AlertDescription></Alert> : <Table><TableHeader><TableRow><TableHead>Account</TableHead><TableHead>Decision</TableHead><TableHead>Locale</TableHead><TableHead>At</TableHead></TableRow></TableHeader><TableBody>{cq.isPending ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">Loading…</TableCell></TableRow> : !consents.length ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">No consents recorded for this version.</TableCell></TableRow> : consents.map((c, i) => <TableRow key={String(c.account_id ?? i)}><TableCell><Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(c.account_id) }}>{String(c.account_id).slice(0, 8)}…</Link></TableCell><TableCell>{c.accepted ? <Badge variant="secondary">accepted</Badge> : <Badge variant="destructive">declined</Badge>}</TableCell><TableCell className="text-xs">{String(c.locale ?? '')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(c.at ?? c.created_at)}</TableCell></TableRow>)}</TableBody></Table>}
      </FormDialog>
    </div>
  )
}
