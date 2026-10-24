import { useState } from 'react'
import { createFileRoute, Link } from '@tanstack/react-router'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** Published content versions (items, store, quests, tables… as one bundle): per-version row counts and the delta to the previous version, export of the current bundle, and import with a dry run that reports what would change before anything is published. */
export const Route = createFileRoute('/_app/content/bundles/')({ component: Versions })

const ts = (v: unknown) => (v ? String(v).replace('T', ' ').slice(0, 19) : '—')
const counts = (r: Row) => (r.counts ?? {}) as Record<string, unknown>
const total = (r: Row) => Object.values(counts(r)).reduce((a: number, b) => a + Number(b || 0), 0)
function delta(a: Row, b?: Row) {
  if (!b) return ''
  const out: string[] = []
  for (const k of new Set([...Object.keys(counts(a)), ...Object.keys(counts(b))])) { const d = Number(counts(a)[k] ?? 0) - Number(counts(b)[k] ?? 0); if (d) out.push(`${k} ${d > 0 ? '+' : ''}${d}`) }
  return out.join(', ')
}

function Versions() {
  const q = useSub<Row>('/v1/admin/content/versions')
  const rows = listOf(q.data, 'versions')
  const [dlg, setDlg] = useState(false)
  const [text, setText] = useState('')
  const [note, setNote] = useState('')
  const [dry, setDry] = useState(true)
  const [prune, setPrune] = useState(false)
  const [result, setResult] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']])
  let parsed: unknown = null, parseErr = ''
  try { parsed = text ? JSON.parse(text) : null } catch (e) { parseErr = (e as Error).message }
  const exportBundle = async () => {
    const r = await raw<Row>('GET', '/v1/admin/content')
    const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob([JSON.stringify(r, null, 2)], { type: 'application/json' })); a.download = `content-${new Date().toISOString().slice(0, 10)}.json`; a.click()
  }
  const bundle = parsed && typeof parsed === 'object' && 'bundle' in (parsed as Row) ? (parsed as Row).bundle : parsed
  const env = bundle && typeof bundle === 'object' ? (bundle as Row).environment : undefined
  const doImport = () => run(async () => { const r = await raw('POST', '/v1/admin/content/import', { bundle, dry_run: dry, prune, note, environment: env ?? null }); setResult(r); if (!dry) { setDlg(false); setText(''); setResult(null) } }, dry ? 'Dry run complete — nothing published' : 'Bundle imported as a new version')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'Versions' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Content versions</h1><p className="mt-1 text-sm text-muted-foreground">Every publish of items, store, quests, reward tables and other game tables is one immutable bundle version. Open a version to see the table-by-table diff; roll back from there.</p></div>
        <div className="flex gap-2"><Button size="sm" variant="outline" onClick={() => void exportBundle()}>Export current</Button><Button size="sm" onClick={() => { setResult(null); setDlg(true) }}>Import bundle</Button></div>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Version</TableHead><TableHead>Published</TableHead><TableHead>Note</TableHead><TableHead className="text-right">Rows</TableHead><TableHead>Change vs previous</TableHead></TableRow></TableHeader>
          <TableBody>{q.isPending ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">Loading…</TableCell></TableRow> : !rows.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">No versions yet — the first import or any edit of a content table publishes v1.</TableCell></TableRow> :
            rows.map((r, i) => <TableRow key={String(r.id)}><TableCell><Link className="font-mono underline" to="/content/bundles/$id" params={{ id: String(r.id) }}>v{String(r.id)}</Link>{i === 0 && <Badge className="ml-2">current</Badge>}</TableCell><TableCell className="whitespace-nowrap text-muted-foreground">{ts(r.created_at)}</TableCell><TableCell className="max-w-[16rem] truncate">{String(r.note ?? '') || '—'}</TableCell><TableCell className="text-right tabular-nums">{total(r)}</TableCell><TableCell className="text-xs text-muted-foreground">{delta(r, rows[i + 1]) || (i === rows.length - 1 ? 'first version' : 'counts unchanged')}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg} onOpenChange={setDlg} title="Import content bundle" submit={dry ? 'Run dry run' : 'Publish'} destructive={!dry} pending={pending} disabled={!parsed || !!parseErr} wide
        onSubmit={doImport}>
        <Field label="Bundle JSON" hint={parseErr || 'Paste a bundle exported from this or another environment, or pick a file.'}>
          <Input type="file" accept="application/json,.json" onChange={(e) => { const f = e.target.files?.[0]; if (f) void f.text().then(setText) }} />
          <Textarea rows={8} className="mt-2 font-mono text-xs" value={text} onChange={(e) => { setText(e.target.value); setResult(null) }} />
        </Field>
        <Field label="Note"><Input value={note} onChange={(e) => setNote(e.target.value)} placeholder="what changed, ticket, …" /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={dry} onCheckedChange={(c) => setDry(!!c)} />Dry run — validate and report changes without publishing</label>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={prune} onCheckedChange={(c) => setPrune(!!c)} />Prune — delete rows of the bundled tables that the bundle does not list</label>
        {env ? <p className="text-xs text-muted-foreground">Bundle exported from environment <b>{String(env)}</b>; the node refuses it if this cluster is a different one.</p> : null}
        {result != null && <div><div className="mb-1 text-xs font-medium">{dry ? 'Dry run result' : 'Result'}</div><JsonView value={result} /></div>}
      </FormDialog>
    </div>
  )
}
