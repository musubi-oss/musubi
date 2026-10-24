import { createFileRoute } from '@tanstack/react-router'
import { useQueries } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
import { listOf } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Who may log in: access policy (open / allowlist / existing / closed, windows, bypasses), the login queue, the allowlist and recent denials. */
export const Route = createFileRoute('/_app/players/access')({ component: Access })

const toLocal = (v: unknown) => (v ? new Date(typeof v === 'number' ? v * 1000 : String(v)).toISOString().slice(0, 16) : '')
const listOfStr = (s: string) => s.split(',').map((x) => x.trim()).filter(Boolean)
const KINDS: [string, string][] = [['account', 'account id'], ['email', 'e-mail'], ['provider', 'identity provider'], ['domain', 'e-mail domain']]

function Access() {
  const [polQ, allowQ, denQ, queueQ] = useQueries({
    queries: [
      { queryKey: ['access', 'policy'], queryFn: ({ signal }: { signal: AbortSignal }) => raw<Row>('GET', '/v1/admin/access/policy', undefined, { signal }) },
      { queryKey: ['access', 'allowlist'], queryFn: ({ signal }: { signal: AbortSignal }) => raw<Row>('GET', '/v1/admin/access/allowlist', undefined, { signal }) },
      { queryKey: ['access', 'denials'], queryFn: ({ signal }: { signal: AbortSignal }) => raw<Row>('GET', '/v1/admin/access/denials', undefined, { signal }) },
      { queryKey: ['access', 'queue'], queryFn: ({ signal }: { signal: AbortSignal }) => raw<Row>('GET', '/v1/admin/auth/queue', undefined, { signal }), refetchInterval: 5000 },
    ],
  })
  const [pol, setPol] = useState<Row>({})
  const [windows, setWindows] = useState<{ from: string; to: string }[]>([])
  const [bypassA, setBypassA] = useState(''); const [bypassP, setBypassP] = useState('')
  const [dirty, setDirty] = useState(false)
  useEffect(() => {
    const p = (polQ.data?.policy ?? {}) as Row
    setPol({ mode: 'open', message: '', allow_existing_accounts: true, kick_on_change: false, ...p })
    setWindows(((p.windows ?? []) as Row[]).map((w) => ({ from: toLocal(w.from ?? w.starts_at), to: toLocal(w.to ?? w.ends_at) })))
    setBypassA(((p.bypass_audiences ?? []) as string[]).join(', ')); setBypassP(((p.bypass_providers ?? []) as string[]).join(', ')); setDirty(false)
  }, [polQ.data])
  const setP = (k: string, v: unknown) => { setPol((p) => ({ ...p, [k]: v })); setDirty(true) }
  const eff = polQ.data?.effective as Row | undefined
  const queue = queueQ.data
  const allow = listOf(allowQ.data, 'entries')
  const denials = listOf(denQ.data, 'denials')
  const [dlg, setDlg] = useState<'' | 'add' | 'import'>('')
  const [form, setForm] = useState<Row>({})
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))
  const { run, pending } = useAct([['access']], () => setDlg(''))
  const savePolicy = () => run(() => raw('PUT', '/v1/admin/access/policy', { ...pol, windows: windows.filter((w) => w.from && w.to).map((w) => ({ from: Math.floor(new Date(w.from).getTime() / 1000), to: Math.floor(new Date(w.to).getTime() / 1000) })), bypass_audiences: listOfStr(bypassA), bypass_providers: listOfStr(bypassP) }), 'Policy saved')

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Access & login' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="text-xl font-semibold">Access &amp; login</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">Who may log in right now, the admission queue, the allowlist and recent denials.{eff && <Badge variant={eff.mode === 'open' ? 'secondary' : 'destructive'}>effective: {String(eff.mode)}{eff.next_change_at ? ` · changes ${ts(eff.next_change_at)}` : ''}</Badge>}</div>
        </div>
        <Button size="sm" variant="outline" onClick={() => confirmThen('Kick everyone the current policy does not admit?', () => run(() => raw('POST', '/v1/admin/access/enforce'), 'Enforced'))}>Enforce now</Button>
        <Button size="sm" disabled={!dirty} onClick={savePolicy}>Save policy</Button>
      </div>
      <Card><CardContent className="pt-6">
        <SubHead title="Access policy" />
        <KvGrid className="xl:grid-cols-2">
          <Kv k="Mode" v={<Select value={String(pol.mode ?? 'open')} onValueChange={(v) => setP('mode', v)}><SelectTrigger className="w-full max-w-full [&>span]:truncate"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="open">open — anyone may log in</SelectItem><SelectItem value="allowlist">allowlist — only listed accounts / e-mails / providers</SelectItem><SelectItem value="existing">existing — no new accounts</SelectItem><SelectItem value="closed">closed — nobody (maintenance)</SelectItem></SelectContent></Select>} />
          <Kv k="Message" v={<Input value={String(pol.message ?? '')} placeholder="shown to players who are turned away" onChange={(e) => setP('message', e.target.value)} />} />
          <Kv k="Flags" v={<div className="space-y-1.5"><label className="flex items-center gap-2"><Checkbox checked={!!pol.allow_existing_accounts} onCheckedChange={(v) => setP('allow_existing_accounts', v === true)} /> existing accounts bypass the allowlist</label><label className="flex items-center gap-2"><Checkbox checked={!!pol.kick_on_change} onCheckedChange={(v) => setP('kick_on_change', v === true)} /> kick sessions that stop being admitted when the policy changes</label></div>} />
          <Kv k="Bypass" v={<div className="space-y-1.5"><Input value={bypassA} placeholder="audiences (comma-separated)" onChange={(e) => { setBypassA(e.target.value); setDirty(true) }} /><Input value={bypassP} placeholder="identity providers (comma-separated)" onChange={(e) => { setBypassP(e.target.value); setDirty(true) }} /></div>} />
          <Kv k="Windows" v={<div className="space-y-1.5">
            <p className="text-xs text-muted-foreground">policy applies only inside; empty = always</p>
            {windows.map((w, i) => <div key={i} className="flex flex-wrap gap-1.5"><Input className="w-52" type="datetime-local" value={w.from} onChange={(e) => { setWindows((ws) => ws.map((x, j) => (j === i ? { ...x, from: e.target.value } : x))); setDirty(true) }} /><Input className="w-52" type="datetime-local" value={w.to} onChange={(e) => { setWindows((ws) => ws.map((x, j) => (j === i ? { ...x, to: e.target.value } : x))); setDirty(true) }} /><Button size="sm" variant="ghost" onClick={() => { setWindows((ws) => ws.filter((_, j) => j !== i)); setDirty(true) }}>×</Button></div>)}
            <Button size="sm" variant="outline" onClick={() => { setWindows((ws) => [...ws, { from: '', to: '' }]); setDirty(true) }}>Add window</Button>
          </div>} />
        </KvGrid>

        <SubHead title="Login queue">
          <span className="self-center text-xs text-muted-foreground">admission when capacity is reached</span>
          <Button size="sm" variant="outline" onClick={() => confirmThen('Drop every waiting ticket?', () => run(() => raw('DELETE', '/v1/admin/auth/queue'), 'Queue flushed'))}>Flush</Button>
        </SubHead>
        {queue ? <KvGrid><Kv k="Waiting" v={queue.waiting} /><Kv k="Online / capacity" v={`${queue.online ?? 0} / ${queue.capacity || '∞'}`} /><Kv k="Admit per second" v={queue.admit_per_sec} /><Kv k="Admitted total" v={queue.admitted} /><Kv k="Longest wait" v={queue.max_wait_secs != null ? `${queue.max_wait_secs}s` : undefined} /></KvGrid> : <p className="text-sm text-muted-foreground">Queue is off — no login capacity configured.</p>}

        <SubHead title="Allowlist" count={allow.length}>
          <Button size="sm" onClick={() => { setForm({ kind: 'account', value: '', note: '' }); setDlg('add') }}>Add</Button>
          <Button size="sm" variant="outline" onClick={() => { setForm({ kind: 'email', values: '', note: '' }); setDlg('import') }}>Import</Button>
        </SubHead>
        <DataTable id="access-allow" rows={allow} loading={allowQ.isPending} empty="Empty — in allowlist mode nobody new gets in"
          cols={[{ key: 'kind' }, { key: 'value', mono: true }, { key: 'note', dim: true }, { key: 'created_at', label: 'added', dim: true, fmt: (e) => ts(e.created_at ?? e.added_at) }]}
          actions={(e) => <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `/v1/admin/access/allowlist/${encodeURIComponent(String(e.kind))}/${encodeURIComponent(String(e.value))}`), 'Removed')}>Remove</Button>} />
        <SubHead title="Recent denials" count={denials.length} />
        <DataTable id="access-denials" rows={denials} loading={denQ.isPending} empty="Nobody has been denied"
          cols={[{ key: 'at', dim: true, fmt: (d) => ts(d.at) }, { key: 'account_id', label: 'account', mono: true, fmt: (d) => d.account_id ?? d.subject ?? '—' }, { key: 'provider' }, { key: 'reason', dim: true }, { key: 'mode', dim: true }, { key: 'ip', mono: true }]} />
      </CardContent></Card>

      <FormDialog open={dlg === 'add'} onOpenChange={(o) => !o && setDlg('')} title="Allowlist entry" submit="Add" pending={pending} disabled={!form.value}
        onSubmit={() => run(() => raw('POST', '/v1/admin/access/allowlist', { kind: form.kind, value: String(form.value).trim(), note: form.note || undefined }), 'Added')}>
        <Field label="Kind"><Select value={String(form.kind)} onValueChange={(v) => set('kind', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{KINDS.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Value"><Input className="font-mono" value={form.value ?? ''} onChange={(e) => set('value', e.target.value)} /></Field>
        <Field label="Note"><Input value={form.note ?? ''} onChange={(e) => set('note', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'import'} onOpenChange={(o) => !o && setDlg('')} title="Import allowlist" submit="Import" pending={pending} disabled={!form.values}
        onSubmit={() => run(() => raw('POST', '/v1/admin/access/allowlist/import', { kind: form.kind, values: String(form.values).split(/[\s,;]+/).filter(Boolean), note: form.note || undefined }), 'Imported')}>
        <Field label="Kind"><Select value={String(form.kind)} onValueChange={(v) => set('kind', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{KINDS.map(([v, l]) => <SelectItem key={v} value={v}>{l}s</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Values (one per line or comma-separated)"><Textarea className="min-h-40 font-mono text-xs" value={form.values ?? ''} onChange={(e) => set('values', e.target.value)} /></Field>
        <Field label="Note"><Input value={form.note ?? ''} onChange={(e) => set('note', e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
