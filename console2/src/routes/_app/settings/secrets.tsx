import { useState } from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** Credentials at rest (webhook secrets, provider client secrets, …): which master key seals them, how many rows are plaintext or sealed with a key this node no longer has, and the re-encryption sweep after a key rotation. Retention and dormant-account sweeps live here too. */
export const Route = createFileRoute('/_app/settings/secrets')({ component: Secrets })

const SWEEPS: { k: string; path: string; title: string; desc: string; confirm?: string }[] = [
  { k: 'reencrypt', path: '/v1/admin/secrets/reencrypt', title: 'Re-encrypt secrets', desc: 'Re-seals every stored secret with the current master key. Run after rotating MUSUBI_MASTER_KEY while the previous key is still in MUSUBI_MASTER_KEY_PREVIOUS.' },
  { k: 'retention', path: '/v1/admin/retention/run', title: 'Run retention', desc: 'Applies the retention policy now (old events, logs, audit, replays) instead of waiting for the nightly job.' },
  { k: 'dormant', path: '/v1/admin/accounts/dormant/sweep', title: 'Dormant accounts', desc: 'Flags accounts past the dormant threshold from the accounts policy and schedules deletion where the policy says so.', confirm: 'Sweep dormant accounts now? Accounts past the policy threshold get a deletion scheduled.' },
  { k: 'deletions', path: '/v1/admin/deletions/sweep', title: 'Execute deletions', desc: 'Erases accounts whose grace period has ended. Irreversible for those accounts.', confirm: 'Execute due deletions now? Account data past the grace period is erased permanently.' },
]

function Secrets() {
  const q = useSub<Row>('/v1/admin/secrets/status')
  const stores = (q.data?.stores as Row[] | undefined) ?? []
  const total = (f: string) => stores.reduce((a, s) => a + Number(s[f] ?? 0), 0)
  const [out, setOut] = useState<Record<string, unknown>>({})
  const [busy, setBusy] = useState('')
  const { run, pending } = useAct([['sub']], () => setBusy(''))
  const go = (s: (typeof SWEEPS)[number]) => { setBusy(s.k); run(async () => setOut((o) => ({ ...o, [s.k]: null })) ?? raw('POST', s.path).then((r) => setOut((o) => ({ ...o, [s.k]: r }))), `${s.title}: done`) }
  const stale = total('stale'), plain = total('plaintext')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Secrets & sweeps' }]} />
      <div><h1 className="text-xl font-semibold">Secrets & sweeps</h1><p className="mt-1 text-sm text-muted-foreground">Secrets the node stores for you are sealed with the master key from the environment. If rows are plaintext (written before a key was configured) or sealed with a key this node no longer has, re-encrypt them. Nothing here shows the secrets themselves.</p></div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {[['Master key', q.data ? (q.data.master_key_configured ?? q.data.configured ? `configured${q.data.key_id ? ` · ${String(q.data.key_id)}` : ''}` : 'not set') : '—'], ['Sealed rows', String(total('sealed'))], ['Plaintext rows', String(plain)], ['Stale key rows', String(stale)]].map(([k, v]) => <Card key={k}><CardContent className="p-4"><div className="text-xs text-muted-foreground">{k}</div><div className={`truncate text-lg font-semibold ${(k === 'Plaintext rows' && plain > 0) || (k === 'Stale key rows' && stale > 0) ? 'text-destructive' : ''}`}>{v}</div></CardContent></Card>)}
      </div>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Stores</CardTitle></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Store</TableHead><TableHead className="text-right">Sealed</TableHead><TableHead className="text-right">Plaintext</TableHead><TableHead className="text-right">Stale key</TableHead></TableRow></TableHeader>
          <TableBody>{!stores.length ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">{q.isPending ? 'Loading…' : 'No secret stores reported.'}</TableCell></TableRow> : stores.map((s) => <TableRow key={String(s.name ?? s.store)}><TableCell className="font-mono text-xs">{String(s.name ?? s.store)}</TableCell><TableCell className="text-right tabular-nums">{String(s.sealed ?? 0)}</TableCell><TableCell className="text-right tabular-nums">{Number(s.plaintext ?? 0) > 0 ? <Badge variant="destructive">{String(s.plaintext)}</Badge> : '0'}</TableCell><TableCell className="text-right tabular-nums">{Number(s.stale ?? 0) > 0 ? <Badge variant="destructive">{String(s.stale)}</Badge> : '0'}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        {SWEEPS.map((s) => <Card key={s.k}><CardHeader className="pb-2"><CardTitle className="text-base">{s.title}</CardTitle><p className="text-sm text-muted-foreground">{s.desc}</p></CardHeader><CardContent className="space-y-2">
          <Button size="sm" variant={s.confirm ? 'destructive' : 'outline'} disabled={pending} onClick={() => (s.confirm ? confirmThen(s.confirm, () => go(s)) : go(s))}>{busy === s.k && pending ? 'Running…' : 'Run now'}</Button>
          {out[s.k] != null && <JsonView value={out[s.k]} />}
        </CardContent></Card>)}
      </div>
    </div>
  )
}
