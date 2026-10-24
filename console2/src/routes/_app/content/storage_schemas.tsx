import { useState } from 'react'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** JSON-Schema validation and secondary indexes for player storage collections; a versioned schema lets you run a migration script over existing objects written under an older version. */
export const Route = createFileRoute('/_app/content/storage_schemas')({
  validateSearch: (s: Record<string, unknown>): { q?: string } => ({ q: typeof s.q === 'string' && s.q ? s.q : undefined }),
  component: Schemas,
})

type Idx = { field: string; kind: string; unique: boolean }
const S = '/v1/admin/storage_schemas'
const KINDS = ['string', 'number', 'bool', 'timestamp']

function Schemas() {
  const { q = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const list = useSub<Row>(S)
  const schemas = listOf(list.data, 'schemas')
  const view = schemas.filter((x) => !q || String(x.collection).toLowerCase().includes(q.toLowerCase()))
  const [dlg, setDlg] = useState('')
  const [cur, setCur] = useState<Row>({})
  const [schemaTxt, setSchemaTxt] = useState('')
  const [idx, setIdx] = useState<Idx[]>([])
  const [mig, setMig] = useState({ from_version: '1', script: '' })
  const [migOut, setMigOut] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const migrate = useAct([['sub']])
  let schemaErr = ''
  try { JSON.parse(schemaTxt || '{}') } catch (e) { schemaErr = (e as Error).message }
  const edit = (x?: Row) => {
    setCur(x ? { ...x } : { collection: '', version: 1 })
    setSchemaTxt(JSON.stringify(x?.schema ?? { type: 'object', properties: {}, required: [] }, null, 2))
    setIdx(((x?.indexes as Row[] | undefined) ?? []).map((i) => ({ field: String(i.field ?? ''), kind: String(i.kind ?? 'string'), unique: !!i.unique })))
    setDlg('edit')
  }
  const save = () => run(() => raw('PUT', `${S}/${encodeURIComponent(String(cur.collection))}`, { version: Number(cur.version) || 1, schema: JSON.parse(schemaTxt || '{}'), indexes: idx.filter((i) => i.field) }), 'Schema saved')
  const props = (x: Row) => Object.keys(((x.schema as Row | undefined)?.properties as Row | undefined) ?? {})
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'Storage schemas' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Storage schemas</h1><p className="mt-1 text-sm text-muted-foreground">Collections without a schema accept any JSON. With one, writes are validated against the JSON Schema and the listed fields get secondary indexes (queryable from the client storage API). Bump the version when the shape changes and migrate old objects with a script.</p></div>
        <Button size="sm" onClick={() => edit()}>New schema</Button>
      </div>
      <Input className="max-w-xs" placeholder="Filter collections…" value={q} onChange={(e) => void nav({ search: { q: e.target.value || undefined }, replace: true })} />
      {list.isError && <Alert variant="destructive"><AlertDescription>{String(list.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Collection</TableHead><TableHead>Version</TableHead><TableHead>Fields</TableHead><TableHead>Indexes</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{list.isPending ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">Loading…</TableCell></TableRow> : !view.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">{q ? 'Nothing matches.' : 'No schemas — every collection is free-form.'}</TableCell></TableRow> :
            view.map((x) => <TableRow key={String(x.collection)}><TableCell><button className="font-mono underline" onClick={() => edit(x)}>{String(x.collection)}</button></TableCell><TableCell>v{String(x.version ?? 1)}</TableCell><TableCell className="max-w-[20rem] truncate text-xs text-muted-foreground">{props(x).join(', ') || '—'}</TableCell><TableCell className="text-xs">{((x.indexes as Row[] | undefined) ?? []).map((i) => <Badge key={String(i.field)} variant="outline" className="mr-1">{String(i.field)}{i.unique ? ' !' : ''}</Badge>)}</TableCell>
              <TableCell className="whitespace-nowrap text-right"><Button size="sm" variant="ghost" onClick={() => { setCur(x); setMig({ from_version: String(Math.max(1, Number(x.version ?? 1) - 1)), script: '' }); setMigOut(null); setDlg('migrate') }}>Migrate</Button><Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen(`Remove schema for ${String(x.collection)}? Existing objects stay; new writes are no longer validated and indexes are dropped.`, () => run(() => raw('DELETE', `${S}/${encodeURIComponent(String(x.collection))}`), 'Schema removed'))}>Delete</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg === 'edit'} onOpenChange={(o) => !o && setDlg('')} title={cur.collection && schemas.some((s) => s.collection === cur.collection) ? `Schema · ${String(cur.collection)}` : 'New storage schema'} pending={pending} disabled={!cur.collection || !!schemaErr} wide onSubmit={save}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-[1fr_8rem]">
          <Field label="Collection"><Input className="font-mono" value={String(cur.collection ?? '')} onChange={(e) => setCur((c) => ({ ...c, collection: e.target.value }))} /></Field>
          <Field label="Version"><Input type="number" min={1} value={String(cur.version ?? 1)} onChange={(e) => setCur((c) => ({ ...c, version: e.target.value }))} /></Field>
        </div>
        <Field label="JSON Schema" hint={schemaErr || 'Draft 2020-12; objects failing validation are rejected on write.'}><Textarea rows={10} className="font-mono text-xs" value={schemaTxt} onChange={(e) => setSchemaTxt(e.target.value)} /></Field>
        <Field label="Indexes">
          <div className="space-y-2">{idx.map((i, n) => <div key={n} className="flex min-w-0 flex-wrap items-center gap-2"><Input className="min-w-0 flex-1 font-mono" placeholder="field.path" value={i.field} onChange={(e) => setIdx((a) => a.map((x, j) => (j === n ? { ...x, field: e.target.value } : x)))} /><Select value={i.kind} onValueChange={(v) => setIdx((a) => a.map((x, j) => (j === n ? { ...x, kind: v } : x)))}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger><SelectContent>{KINDS.map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent></Select><label className="flex items-center gap-1 text-xs"><Checkbox checked={i.unique} onCheckedChange={(c) => setIdx((a) => a.map((x, j) => (j === n ? { ...x, unique: !!c } : x)))} />unique</label><Button type="button" size="sm" variant="ghost" onClick={() => setIdx((a) => a.filter((_, j) => j !== n))}>×</Button></div>)}
            <Button type="button" size="sm" variant="outline" onClick={() => setIdx((a) => [...a, { field: '', kind: 'string', unique: false }])}>Add index</Button></div>
        </Field>
      </FormDialog>
      <FormDialog open={dlg === 'migrate'} onOpenChange={(o) => !o && setDlg('')} title={`Migrate · ${String(cur.collection ?? '')}`} submit="Run migration" destructive pending={migrate.pending} disabled={!mig.script}
        onSubmit={() => migrate.run(async () => { const r = await raw<Row>('POST', `${S}/${encodeURIComponent(String(cur.collection))}/migrate`, { from_version: Number(mig.from_version) || 1, script: mig.script }); setMigOut(r) }, 'Migration finished')}>
        <p className="text-sm text-muted-foreground">Runs the script over every object of this collection stored under the given version and rewrites it as v{String(cur.version ?? '')}. The script receives <code>obj</code> and returns the new object.</p>
        <Field label="From version"><Input type="number" min={1} value={mig.from_version} onChange={(e) => setMig((m) => ({ ...m, from_version: e.target.value }))} /></Field>
        <Field label="Script"><Textarea rows={6} className="font-mono text-xs" placeholder={'obj.level = obj.lvl ?? 1; delete obj.lvl; return obj'} value={mig.script} onChange={(e) => setMig((m) => ({ ...m, script: e.target.value }))} /></Field>
        {migOut != null && <div><div className="mb-1 text-xs font-medium">Result{(migOut as Row).migrated != null ? ` — ${String((migOut as Row).migrated)} objects` : ''}</div><JsonView value={migOut} /></div>}
      </FormDialog>
    </div>
  )
}
