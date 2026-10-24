import { useState } from 'react'
import { createFileRoute } from '@tanstack/react-router'
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
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** Other clusters of this game (dev → staging → prod) reachable by URL, and promotions: exporting this cluster's content sections into one of them, with a dry run first. */
export const Route = createFileRoute('/_app/settings/environments')({ component: Environments })

const E = '/v1/admin/environments'
const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))

function Environments() {
  const eq = useSub<Row>(E)
  const envs = listOf(eq.data, 'environments')
  const pq = useSub<Row>('/v1/admin/promotions?limit=50')
  const promos = listOf(pq.data, 'promotions')
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const [status, setStatus] = useState<Record<string, unknown>>({})
  const [result, setResult] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const check = async (name: string) => { let v: unknown; try { v = await raw('GET', `${E}/${encodeURIComponent(name)}/status`) } catch (e) { v = { error: (e as Error).message } } setStatus((s) => ({ ...s, [name]: v })) }
  const openEnv = (r?: Row) => { setF(r ? { name: r.name, url: r.url, protected: !!r.protected, description: r.description ?? '', existing: true } : { name: '', url: '', protected: false, description: '' }); setDlg('env') }
  const openPromote = (r: Row) => { setF({ to: r.name, dry_run: true, prune: false, note: '' }); setResult(null); setDlg('promote') }
  const promote = () => keep.run(async () => { const r = await raw('POST', `${E}/${encodeURIComponent(String(f.to))}/promote`, { dry_run: !!f.dry_run, prune: !!f.prune, note: f.note }); setResult(r); if (!f.dry_run) setDlg('') }, f.dry_run ? 'Dry run finished — nothing was written' : `Promoted to ${String(f.to)}`)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Environments' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Environments</h1><p className="mt-1 text-sm text-muted-foreground">Each row is another Musubi cluster. Promotion exports this cluster's content (items, store, quests, configs…) as a bundle and imports it there — the same path as Content → Versions, so it lands as a new version that can be rolled back. Protected environments require a dry run first.</p></div>
        <Button size="sm" onClick={() => openEnv()}>Add environment</Button>
      </div>
      {eq.isError && <Alert variant="destructive"><AlertDescription>{String(eq.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Name</TableHead><TableHead>URL</TableHead><TableHead>Description</TableHead><TableHead>Status</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{eq.isPending ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">Loading…</TableCell></TableRow> : !envs.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">No other environments registered. Add staging or production here to promote content to it.</TableCell></TableRow> :
            envs.map((r) => { const name = String(r.name); const st = status[name] as Row | undefined; return <TableRow key={name}><TableCell><button className="font-mono underline" onClick={() => openEnv(r)}>{name}</button>{r.protected ? <Badge variant="outline" className="ml-2">protected</Badge> : null}</TableCell><TableCell className="max-w-[18rem] truncate font-mono text-xs">{String(r.url ?? '')}</TableCell><TableCell className="max-w-[16rem] truncate text-sm text-muted-foreground">{String(r.description ?? '')}</TableCell>
              <TableCell className="text-xs">{st ? (st.error ? <span className="text-destructive">{String(st.error)}</span> : <span className="text-emerald-600">reachable{st.version ? ` · ${String(st.version)}` : ''}{st.content_version != null ? ` · content v${String(st.content_version)}` : ''}</span>) : <Button size="sm" variant="ghost" onClick={() => void check(name)}>Check</Button>}</TableCell>
              <TableCell className="whitespace-nowrap text-right"><Button size="sm" variant="ghost" onClick={() => openPromote(r)}>Promote…</Button><Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => confirmThen(`Remove environment ${name}? The remote cluster is not touched.`, () => keep.run(() => raw('DELETE', `${E}/${encodeURIComponent(name)}`), 'Environment removed'))}>Remove</Button></TableCell></TableRow> })}</TableBody></Table>
      </CardContent></Card>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Promotion history</CardTitle></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>When</TableHead><TableHead>To</TableHead><TableHead>By</TableHead><TableHead>Note</TableHead><TableHead>Result</TableHead></TableRow></TableHeader>
          <TableBody>{!promos.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">{pq.isPending ? 'Loading…' : 'No promotions yet.'}</TableCell></TableRow> : promos.map((p, i) => <TableRow key={String(p.id ?? i)}><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(p.created_at ?? p.at)}</TableCell><TableCell className="font-mono text-xs">{String(p.to ?? p.environment ?? '')}</TableCell><TableCell className="text-xs">{String(p.operator ?? p.by ?? '—')}</TableCell><TableCell className="max-w-[16rem] truncate text-sm">{String(p.note ?? '')}</TableCell><TableCell className="text-xs">{p.dry_run ? <Badge variant="outline">dry run</Badge> : p.error ? <span className="text-destructive">{String(p.error)}</span> : <span className="text-emerald-600">{p.remote_version != null ? `content v${String(p.remote_version)}` : 'ok'}</span>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg === 'env'} onOpenChange={(o) => !o && setDlg('')} title={f.existing ? `Environment · ${String(f.name)}` : 'Add environment'} pending={pending} disabled={!f.name || !f.url} onSubmit={() => run(() => raw('PUT', `${E}/${encodeURIComponent(String(f.name))}`, { url: f.url, protected: !!f.protected, description: f.description || undefined }), 'Environment saved')}>
        <Field label="Name"><Input className="font-mono" value={String(f.name ?? '')} disabled={!!f.existing} onChange={(e) => setF((x) => ({ ...x, name: e.target.value }))} placeholder="staging" /></Field>
        <Field label="API URL" hint="Base URL of the other cluster; its server key is taken from MUSUBI_ENV_<NAME>_KEY on this node."><Input value={String(f.url ?? '')} onChange={(e) => setF((x) => ({ ...x, url: e.target.value }))} placeholder="https://staging.example.com" /></Field>
        <Field label="Description"><Input value={String(f.description ?? '')} onChange={(e) => setF((x) => ({ ...x, description: e.target.value }))} /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!f.protected} onCheckedChange={(c) => setF((x) => ({ ...x, protected: !!c }))} />Protected — promotions need a dry run first and are logged with the operator</label>
      </FormDialog>
      <FormDialog open={dlg === 'promote'} onOpenChange={(o) => !o && setDlg('')} title={`Promote content → ${String(f.to ?? '')}`} submit={f.dry_run ? 'Run dry run' : 'Promote'} destructive={!f.dry_run} pending={keep.pending} wide onSubmit={promote}>
        <p className="text-sm text-muted-foreground">Exports this cluster's content bundle and imports it into <b>{String(f.to ?? '')}</b>. Run the dry run to see what would be added, changed or removed there.</p>
        <Field label="Note"><Input value={String(f.note ?? '')} onChange={(e) => setF((x) => ({ ...x, note: e.target.value }))} placeholder="release 1.4 balance pass" /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!f.dry_run} onCheckedChange={(c) => setF((x) => ({ ...x, dry_run: !!c }))} />Dry run</label>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!f.prune} onCheckedChange={(c) => setF((x) => ({ ...x, prune: !!c }))} />Prune — delete rows on the target that this bundle does not list</label>
        {result != null && <div><div className="mb-1 text-xs font-medium">{f.dry_run ? 'Dry run result' : 'Result'}</div><JsonView value={result} /></div>}
      </FormDialog>
    </div>
  )
}
