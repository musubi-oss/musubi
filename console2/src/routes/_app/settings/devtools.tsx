import { useState } from 'react'
import { createFileRoute, Link } from '@tanstack/react-router'
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
import { Textarea } from '@/components/ui/textarea'

/** Developer tooling for a non-production cluster: one-click demo data, named fixtures applied/dropped as a unit, request recording + replay, developer-flagged accounts and clearing an operator's login lock. */
export const Route = createFileRoute('/_app/settings/devtools')({ component: DevTools })

const D = '/v1/admin/dev'
const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 19))
const NAMES = ['ayla', 'brick', 'cass', 'dorn', 'eiko', 'fenn', 'gale', 'hux', 'iris', 'jude', 'kato', 'lyra']
const demoManifest = () => ({
  accounts: NAMES.map((username, i) => ({
    username, display_name: username[0]!.toUpperCase() + username.slice(1),
    metadata: { platform: ['steam', 'ios', 'android', 'ps5'][i % 4], country: ['DE', 'US', 'BR', 'JP', 'FR', 'KR'][i % 6], level: 3 + i * 4 },
    wallet: { gold: 250 + i * 175, gems: i % 3 === 0 ? 40 + i * 5 : 0 },
    inventory: [{ item_id: 'sword_basic', quantity: 1 }, { item_id: 'potion_small', quantity: 2 + i }, ...(i % 2 ? [{ item_id: 'shield_wood', quantity: 1 }] : [])],
    storage: [{ collection: 'profile', key: 'settings', value: { music: i % 2 === 0, lang: ['en', 'de', 'pt', 'ja'][i % 4] } }],
  })),
})

function DevTools() {
  const rec = useSub<Row>(`${D}/recording`)
  const [recPath, setRecPath] = useState('')
  const recs = useSub<Row>(`${D}/recordings?limit=100${recPath ? `&path=${encodeURIComponent(recPath)}` : ''}`)
  const fx = useSub<Row>(`${D}/fixtures`)
  const dev = useSub<Row>('/v1/admin/developers')
  const recordings = listOf(recs.data, 'recordings'), fixtures = listOf(fx.data, 'fixtures'), developers = listOf(dev.data, 'developers')
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const [lockEmail, setLockEmail] = useState('')
  const [out, setOut] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const r = rec.data ?? {}
  const err = [rec, recs, fx, dev].find((q) => q.isError)
  let manifestErr = ''
  try { JSON.parse(String(f.manifest ?? '{}')) } catch (e) { manifestErr = (e as Error).message }
  const demo = () => keep.run(async () => {
    const quiet = async (p: string, body: unknown) => { try { await raw('POST', p, body) } catch { /* already exists */ } }
    await Promise.all([
      quiet('/v1/server/currencies', { code: 'gold', name: 'Gold', max_balance: null }), quiet('/v1/server/currencies', { code: 'gems', name: 'Gems', max_balance: null }),
      ...([['sword_basic', 'Basic sword', 'weapon', false], ['potion_small', 'Small potion', 'consumable', true], ['shield_wood', 'Wooden shield', 'armor', false]] as [string, string, string, boolean][]).map(([id, name, category, stackable]) => quiet('/v1/server/items', { id, name, category, stackable, max_stack: stackable ? 99 : 1, tradeable: true, unique: false, bind: null, properties: {}, slots: [] })),
    ])
    await raw('POST', `${D}/fixtures`, { name: 'demo', reset: true, ...demoManifest() })
  }, 'Demo data loaded — 2 currencies, 3 items, 12 players with wallets, inventory and storage')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Developer tools' }]} />
      <div><h1 className="text-xl font-semibold">Developer tools</h1><p className="mt-1 text-sm text-muted-foreground">For dev and staging clusters. Fixtures are named data sets you can apply and drop as a unit; request recording captures API calls for replay against another node; developer accounts bypass store/feature gates in the client.</p></div>
      {err && <Alert variant="destructive"><AlertDescription>{String(err.error)}</AlertDescription></Alert>}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Demo data</CardTitle><p className="text-sm text-muted-foreground">An empty node is hard to explore. This creates 2 currencies, 3 items and 12 players with wallets, inventory and storage as fixture <code>demo</code> — drop it below when done.</p></CardHeader><CardContent><Button size="sm" disabled={keep.pending} onClick={demo}>Load demo data</Button></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Developer accounts <Badge variant="outline" className="ml-1">{developers.length}</Badge></CardTitle><p className="text-sm text-muted-foreground">Flag an account from its player page (Actions → Mark developer). Flagged accounts are listed here and can be unflagged.</p></CardHeader><CardContent className="p-0">
          <Table><TableBody>{!developers.length ? <TableRow><TableCell className="text-muted-foreground">No developer accounts.</TableCell></TableRow> : developers.map((d) => <TableRow key={String(d.account_id)}><TableCell><Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(d.account_id) }}>{String(d.account_id).slice(0, 8)}…</Link></TableCell><TableCell className="text-xs">{String(d.note ?? '')}</TableCell><TableCell className="text-xs text-muted-foreground">{String(d.set_by ?? '')} · {ts(d.created_at)}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => keep.run(() => raw('DELETE', `/v1/admin/developers/${String(d.account_id)}`), 'Developer flag removed')}>Unflag</Button></TableCell></TableRow>)}</TableBody></Table>
        </CardContent></Card>
      </div>
      <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Fixtures <Badge variant="outline" className="ml-1">{fixtures.length}</Badge></CardTitle><Button size="sm" variant="outline" onClick={() => { setF({ name: '', reset: false, manifest: JSON.stringify({ accounts: 10 }, null, 2) }); setDlg('fixture') }}>Apply fixture</Button></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Name</TableHead><TableHead>Applied</TableHead><TableHead>Summary</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{!fixtures.length ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">{fx.isPending ? 'Loading…' : 'No fixtures applied.'}</TableCell></TableRow> : fixtures.map((x) => <TableRow key={String(x.name)}><TableCell className="font-mono text-xs">{String(x.name)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(x.applied_at ?? x.created_at)}</TableCell><TableCell className="max-w-[28rem] truncate font-mono text-xs text-muted-foreground">{JSON.stringify(x.summary ?? x.manifest ?? x.counts ?? {})}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => confirmThen(`Drop fixture ${String(x.name)}? Everything it created is deleted.`, () => keep.run(() => raw('DELETE', `${D}/fixtures/${encodeURIComponent(String(x.name))}`), 'Fixture dropped'))}>Drop</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <Card><CardHeader className="pb-2"><div className="flex flex-wrap items-center gap-2"><CardTitle className="text-base">Request recording</CardTitle>{r.enabled ? <Badge variant="destructive">recording</Badge> : <Badge variant="outline">off</Badge>}<div className="ml-auto flex gap-2"><Button size="sm" variant="outline" disabled={keep.pending} onClick={() => keep.run(() => raw('PUT', `${D}/recording`, { enabled: !r.enabled, sample_percent: r.sample_percent ?? 100, path_prefix: r.path_prefix ?? '/v1/' }), r.enabled ? 'Recording stopped' : 'Recording started')}>{r.enabled ? 'Stop' : 'Start'}</Button><Button size="sm" variant="ghost" onClick={() => { setF({ sample_percent: r.sample_percent ?? 100, path_prefix: r.path_prefix ?? '/v1/' }); setDlg('recording') }}>Settings</Button><Button size="sm" variant="ghost" disabled={keep.pending || !recordings.length} onClick={() => confirmThen('Delete all recorded requests?', () => keep.run(() => raw('DELETE', `${D}/recordings`), 'Recordings cleared'))}>Clear</Button></div></div><p className="text-sm text-muted-foreground">Captures {String(r.sample_percent ?? 100)}% of requests under <code>{String(r.path_prefix ?? '/v1/')}</code> with their bodies. Replay re-issues a request against this node — handy for reproducing a bug report.</p></CardHeader><CardContent className="p-0">
        <div className="flex gap-2 border-b p-3"><Input className="w-72 font-mono" placeholder="filter by path" value={recPath} onChange={(e) => setRecPath(e.target.value)} /></div>
        <Table><TableHeader><TableRow><TableHead>At</TableHead><TableHead>Method</TableHead><TableHead>Path</TableHead><TableHead>Status</TableHead><TableHead className="text-right">ms</TableHead><TableHead>Actor</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{!recordings.length ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">{recs.isPending ? 'Loading…' : 'No recorded requests.'}</TableCell></TableRow> : recordings.map((x) => <TableRow key={String(x.id)}><TableCell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{ts(x.at ?? x.created_at)}</TableCell><TableCell className="font-mono text-xs">{String(x.method)}</TableCell><TableCell className="max-w-[20rem] truncate font-mono text-xs">{String(x.path)}</TableCell><TableCell><Badge variant={Number(x.status) >= 400 ? 'destructive' : 'outline'}>{String(x.status ?? '')}</Badge></TableCell><TableCell className="text-right tabular-nums text-xs">{String(x.duration_ms ?? '')}</TableCell><TableCell className="font-mono text-xs text-muted-foreground">{String(x.actor ?? x.account_id ?? '').slice(0, 12)}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => keep.run(async () => setOut(await raw('POST', `${D}/recordings/${String(x.id)}/replay`)), 'Replayed')}>Replay</Button></TableCell></TableRow>)}</TableBody></Table>
        {out != null && <div className="border-t p-3"><div className="mb-1 text-xs font-medium">Replay result</div><JsonView value={out} /></div>}
      </CardContent></Card>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Operator login lock</CardTitle><p className="text-sm text-muted-foreground">After repeated failed password attempts an operator e-mail is locked for a while. Clear it here instead of waiting.</p></CardHeader><CardContent><form className="flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); keep.run(() => raw('DELETE', `/v1/admin/auth/locks/${encodeURIComponent(lockEmail.trim())}`), `Lock cleared for ${lockEmail.trim()}`) }}><Input type="email" className="w-72" placeholder="operator e-mail" value={lockEmail} onChange={(e) => setLockEmail(e.target.value)} /><Button type="submit" size="sm" variant="outline" disabled={!lockEmail.trim() || keep.pending}>Clear lock</Button></form></CardContent></Card>
      <FormDialog open={dlg === 'fixture'} onOpenChange={(o) => !o && setDlg('')} title="Apply fixture" submit="Apply" pending={pending} disabled={!f.name || !!manifestErr} wide onSubmit={() => run(() => raw('POST', `${D}/fixtures`, { name: f.name, reset: !!f.reset, ...(JSON.parse(String(f.manifest ?? '{}')) as Row) }), `Fixture ${String(f.name)} applied`)}>
        <Field label="Name"><Input className="font-mono" value={String(f.name ?? '')} onChange={(e) => setF((x) => ({ ...x, name: e.target.value }))} /></Field>
        <Field label="Manifest (JSON)" hint={manifestErr || 'accounts: N (generated) or a list of {username, display_name, metadata, wallet, inventory, storage}'}><Textarea rows={12} className="font-mono text-xs" value={String(f.manifest ?? '')} onChange={(e) => setF((x) => ({ ...x, manifest: e.target.value }))} /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!f.reset} onCheckedChange={(c) => setF((x) => ({ ...x, reset: !!c }))} />Reset — drop a previous fixture with this name first</label>
      </FormDialog>
      <FormDialog open={dlg === 'recording'} onOpenChange={(o) => !o && setDlg('')} title="Recording settings" pending={pending} onSubmit={() => run(() => raw('PUT', `${D}/recording`, { enabled: !!r.enabled, sample_percent: Number(f.sample_percent) || 100, path_prefix: f.path_prefix || '/v1/' }), 'Recording settings saved')}>
        <Field label="Sample percent"><Input type="number" min={1} max={100} value={String(f.sample_percent ?? 100)} onChange={(e) => setF((x) => ({ ...x, sample_percent: e.target.value }))} /></Field>
        <Field label="Path prefix"><Input className="font-mono" value={String(f.path_prefix ?? '/v1/')} onChange={(e) => setF((x) => ({ ...x, path_prefix: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
