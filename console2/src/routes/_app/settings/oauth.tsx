import { useState } from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** Machine clients for the admin API (CI, LiveOps tooling, partner services): client-credentials with a role and scopes, several secrets per client so they can be rotated without downtime, issued tokens, and IP restrictions. */
export const Route = createFileRoute('/_app/settings/oauth')({ component: OAuthClients })

const O = '/v1/admin/oauth/clients'
const ROLES = ['admin', 'liveops', 'ops', 'readonly', 'server', 'support']
const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const list = (v: unknown) => String(v ?? '').split(/[\n,]/).map((s) => s.trim()).filter(Boolean)
const EMPTY: Row = { name: '', description: '', kind: 'confidential', role: 'readonly', scopes: '', redirect_uris: '', allowed_ips: '', token_ttl_secs: '3600', owner: '' }

function OAuthClients() {
  const q = useSub<Row>(O)
  const clients = listOf(q.data, 'clients')
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>(EMPTY)
  const [made, setMade] = useState<Row | null>(null)
  const [cur, setCur] = useState<string | null>(null)
  const cq = useSub<Row>(cur ? `${O}/${cur}` : null)
  const tq = useSub<Row>(cur ? `${O}/${cur}/tokens?all=true` : null)
  const c = (cq.data?.client as Row | undefined) ?? cq.data ?? null
  const tokens = listOf(tq.data, 'tokens')
  const secrets = (c?.secrets as Row[] | undefined) ?? []
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const create = () => keep.run(async () => { setMade((await raw<Row>('POST', O, { name: f.name, description: f.description || undefined, kind: f.kind, role: f.role, scopes: list(f.scopes), redirect_uris: list(f.redirect_uris), allowed_ips: list(f.allowed_ips), token_ttl_secs: Number(f.token_ttl_secs) || 3600, owner: f.owner || undefined })) ?? {}) }, 'Client created — copy the secret now')
  const rotate = () => { const label = window.prompt('Label for the new secret (the old ones stay valid until deleted)', 'rotated ' + new Date().toISOString().slice(0, 10)); if (label == null || !c) return; keep.run(async () => setMade((await raw<Row>('POST', `${O}/${String(c.client_id)}/secrets`, { label })) ?? {}), 'Secret issued — copy it now, it is shown once') }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'OAuth clients' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">OAuth clients</h1><p className="mt-1 text-sm text-muted-foreground">Non-human access to the API. A confidential client exchanges its id + secret for a short-lived token (<code>POST /v1/oauth/token</code>) carrying its role and scopes; public clients use PKCE. Secrets are shown once, at creation or rotation.</p></div>
        <Button size="sm" onClick={() => { setF(EMPTY); setMade(null); setDlg('create') }}>New client</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Name</TableHead><TableHead>Client id</TableHead><TableHead>Kind</TableHead><TableHead>Role</TableHead><TableHead>Last token</TableHead><TableHead>State</TableHead></TableRow></TableHeader>
          <TableBody>{q.isPending ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Loading…</TableCell></TableRow> : !clients.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">No clients. Create one for CI or a LiveOps tool instead of sharing the server key.</TableCell></TableRow> :
            clients.map((r) => <TableRow key={String(r.client_id ?? r.id)} className="cursor-pointer" onClick={() => { setCur(String(r.client_id ?? r.id)); setMade(null); setDlg('client') }}><TableCell className="font-medium underline">{String(r.name ?? '')}</TableCell><TableCell className="font-mono text-xs">{String(r.client_id ?? r.id)}</TableCell><TableCell className="text-xs">{String(r.kind ?? '')}</TableCell><TableCell><Badge variant="outline">{String(r.role ?? '')}</Badge></TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(r.last_token_at)}</TableCell><TableCell>{r.enabled === false ? <Badge variant="destructive">disabled</Badge> : <Badge variant="secondary">enabled</Badge>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <FormDialog open={dlg === 'create'} onOpenChange={(o) => { if (!o) { setDlg(''); setMade(null) } }} title="New OAuth client" submit={made ? 'Done' : 'Create'} pending={keep.pending} disabled={!made && !f.name} wide onSubmit={() => (made ? setDlg('') : create())}>
        {made ? <div className="space-y-2"><p className="text-sm">Store these now — the secret is not retrievable later.</p><KvGrid><Kv k="client_id" v={String(made.client_id ?? '')} mono /><Kv k="client_secret" v={String(made.client_secret ?? made.secret ?? '(public client — no secret)')} mono /></KvGrid></div> : <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Name"><Input value={String(f.name)} onChange={(e) => setF((x) => ({ ...x, name: e.target.value }))} /></Field>
          <Field label="Owner (team / contact)"><Input value={String(f.owner)} onChange={(e) => setF((x) => ({ ...x, owner: e.target.value }))} /></Field>
          <Field label="Kind"><Select value={String(f.kind)} onValueChange={(v) => setF((x) => ({ ...x, kind: v }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="confidential">confidential (has a secret)</SelectItem><SelectItem value="public">public (PKCE, no secret)</SelectItem></SelectContent></Select></Field>
          <Field label="Role"><Select value={String(f.role)} onValueChange={(v) => setF((x) => ({ ...x, role: v }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent>{ROLES.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Scopes" hint="comma-separated; empty = everything the role allows"><Input className="font-mono" value={String(f.scopes)} onChange={(e) => setF((x) => ({ ...x, scopes: e.target.value }))} /></Field>
          <Field label="Token TTL (seconds)"><Input type="number" min={60} value={String(f.token_ttl_secs)} onChange={(e) => setF((x) => ({ ...x, token_ttl_secs: e.target.value }))} /></Field>
          <Field label="Allowed IPs / CIDRs" hint="comma-separated; empty = any"><Input className="font-mono" value={String(f.allowed_ips)} onChange={(e) => setF((x) => ({ ...x, allowed_ips: e.target.value }))} /></Field>
          <Field label="Redirect URIs (public clients)"><Input className="font-mono" value={String(f.redirect_uris)} onChange={(e) => setF((x) => ({ ...x, redirect_uris: e.target.value }))} /></Field>
          <div className="col-span-full"><Field label="Description"><Input value={String(f.description)} onChange={(e) => setF((x) => ({ ...x, description: e.target.value }))} /></Field></div>
        </div>}
      </FormDialog>
      <FormDialog open={dlg === 'client'} onOpenChange={(o) => { if (!o) { setDlg(''); setCur(null); setMade(null) } }} title={`Client · ${String(c?.name ?? cur ?? '')}`} submit="Close" wide onSubmit={() => { setDlg(''); setCur(null) }}>
        {cq.isError ? <Alert variant="destructive"><AlertDescription>{String(cq.error)}</AlertDescription></Alert> : !c ? <p className="text-sm text-muted-foreground">Loading…</p> : <div className="space-y-4">
          <KvGrid><Kv k="client_id" v={String(c.client_id ?? '')} mono /><Kv k="kind" v={String(c.kind ?? '')} /><Kv k="role" v={String(c.role ?? '')} /><Kv k="owner" v={String(c.owner ?? '—')} /><Kv k="token TTL" v={`${String(c.token_ttl_secs ?? '')} s`} /><Kv k="last token" v={ts(c.last_token_at)} /><Kv k="failures" v={String(c.failures ?? 0)} /><Kv k="enabled" v={c.enabled === false ? 'no' : 'yes'} /><Kv k="scopes" v={((c.scopes as string[] | undefined) ?? []).join(', ') || 'all for role'} /><Kv k="allowed IPs" v={((c.allowed_ips as string[] | undefined) ?? []).join(', ') || 'any'} /><Kv k="redirect URIs" v={((c.redirect_uris as string[] | undefined) ?? []).join(', ') || '—'} /></KvGrid>
          {made && <div className="rounded-md border p-3"><div className="mb-1 text-xs font-medium">New secret — shown once</div><code className="break-all text-xs">{String(made.client_secret ?? made.secret ?? '')}</code></div>}
          <div><div className="mb-1 flex items-center justify-between"><div className="text-sm font-medium">Secrets ({secrets.length})</div>{c.kind !== 'public' && <Button size="sm" variant="outline" disabled={keep.pending} onClick={rotate}>Rotate (add secret)</Button>}</div>
            <Table><TableBody>{!secrets.length ? <TableRow><TableCell className="text-muted-foreground">No secrets.</TableCell></TableRow> : secrets.map((s) => <TableRow key={String(s.id)}><TableCell className="text-xs">{String(s.label ?? s.id)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(s.created_at)}{s.last_used_at ? ` · used ${ts(s.last_used_at)}` : ''}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={keep.pending || secrets.length < 2} onClick={() => confirmThen('Delete this secret? Anything still using it stops authenticating.', () => keep.run(() => raw('DELETE', `${O}/${String(c.client_id)}/secrets/${String(s.id)}`), 'Secret deleted'))}>Delete</Button></TableCell></TableRow>)}</TableBody></Table></div>
          <div><div className="mb-1 text-sm font-medium">Tokens ({tokens.length})</div>{tq.isError ? <p className="text-xs text-muted-foreground">Token list unavailable.</p> : <Table><TableBody>{!tokens.length ? <TableRow><TableCell className="text-muted-foreground">No tokens issued.</TableCell></TableRow> : tokens.slice(0, 20).map((t, i) => <TableRow key={String(t.id ?? i)}><TableCell className="font-mono text-xs">{String(t.jti ?? t.id ?? '').slice(0, 12)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">issued {ts(t.issued_at ?? t.created_at)} · expires {ts(t.expires_at)}</TableCell><TableCell className="text-xs">{t.revoked_at ? <Badge variant="outline">revoked</Badge> : ''}</TableCell></TableRow>)}</TableBody></Table>}</div>
          {c.metadata != null && <JsonView value={c.metadata} />}
          <div className="flex gap-2"><Button size="sm" variant="destructive" disabled={pending} onClick={() => confirmThen(`Delete client ${String(c.name)}? All its secrets and tokens stop working.`, () => { setCur(null); run(() => raw('DELETE', `${O}/${String(c.client_id)}`), 'Client deleted') })}>Delete client</Button></div>
        </div>}
      </FormDialog>
    </div>
  )
}
