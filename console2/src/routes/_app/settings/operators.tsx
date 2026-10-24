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
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** Who can open this console and how: operators (e-mail + password or SSO), pending invites, single sign-on settings and the JWT signing keys behind operator sessions. */
export const Route = createFileRoute('/_app/settings/operators')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: ['invites', 'sso', 'jwt'].includes(String(s.tab)) ? String(s.tab) : undefined }),
  component: Access,
})

const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * 1000) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const ROLES = ['admin', 'operator', 'viewer']
const signIn = (r: Row) => [r.has_password ? 'password' : '', String(r.source ?? r.provider ?? '')].filter(Boolean).join(' + ') || 'key'
const inviteState = (i: Row) => (i.decided_at ? (i.accepted ? 'accepted' : 'declined') : i.expired ? 'expired' : 'pending')

function Access() {
  const { tab = 'operators' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const ops = useSub<Row>('/v1/admin/operators'), inv = useSub<Row>('/v1/admin/operators/invites'), sso = useSub<Row>('/v1/admin/sso'), jwt = useSub<Row>('/v1/admin/jwt/keys')
  const members = listOf(ops.data, 'members'), invites = listOf(inv.data, 'invites'), keys = listOf(jwt.data, 'keys')
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const [made, setMade] = useState<Row | null>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const keep = useAct([['sub']])
  const [ssoF, setSsoF] = useState<Row | null>(null)
  const s = ssoF ?? sso.data ?? {}
  const err = [ops, inv, sso, jwt].find((q) => q.isError)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Operators & access' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Operators & access</h1><p className="mt-1 text-sm text-muted-foreground">Operators sign in with e-mail and password (Argon2id) or through SSO; the server key is the break-glass login. Invites let someone set their own password; JWT keys sign operator sessions — rotate to invalidate nothing immediately but stop issuing with the old key.</p></div>
        <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'operators' ? undefined : v }, replace: true })}><TabsList className="h-auto flex-wrap">{([['operators', `Operators (${members.length})`], ['invites', `Invites (${invites.filter((i) => inviteState(i) === 'pending').length})`], ['sso', 'SSO'], ['jwt', 'JWT keys']] as [string, string][]).map(([v, l]) => <TabsTrigger key={v} value={v}>{l}</TabsTrigger>)}</TabsList></Tabs>
      </div>
      {err && <Alert variant="destructive"><AlertDescription>{String(err.error)}</AlertDescription></Alert>}
      {tab === 'operators' && <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Operators</CardTitle><Button size="sm" onClick={() => { setF({ role: 'operator' }); setDlg('add') }}>Add operator</Button></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>E-mail</TableHead><TableHead>Name</TableHead><TableHead>Role</TableHead><TableHead>Signs in with</TableHead><TableHead>Last login</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{ops.isPending ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">Loading…</TableCell></TableRow> : !members.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">No operators yet — everyone uses the server key. Add one to get personal logins and an audit trail by person.</TableCell></TableRow> :
            members.map((r) => <TableRow key={String(r.id)}><TableCell className="font-mono text-xs">{String(r.email ?? '')}</TableCell><TableCell>{String(r.name ?? '—')}</TableCell><TableCell><Badge variant="outline">{String(r.role ?? '')}</Badge></TableCell><TableCell className="text-xs">{signIn(r)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(r.last_login_at ?? r.last_seen)}</TableCell>
              <TableCell className="whitespace-nowrap text-right"><Button size="sm" variant="ghost" onClick={() => { setF({ id: r.id, email: r.email, password: '' }); setDlg('pw') }}>Reset password</Button><Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen(`Remove ${String(r.email)}? Their sessions are signed out.`, () => keep.run(() => raw('DELETE', `/v1/admin/operators/${encodeURIComponent(String(r.id))}`), 'Operator removed'))}>Remove</Button></TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>}
      {tab === 'invites' && <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Invites</CardTitle><Button size="sm" onClick={() => { setF({ role: 'operator', hours: '72' }); setMade(null); setDlg('invite') }}>Invite</Button></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>E-mail</TableHead><TableHead>Role</TableHead><TableHead>Created</TableHead><TableHead>Expires</TableHead><TableHead>State</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{!invites.length ? <TableRow><TableCell colSpan={6} className="text-muted-foreground">{inv.isPending ? 'Loading…' : 'No invites.'}</TableCell></TableRow> : invites.map((i) => { const st = inviteState(i); return <TableRow key={String(i.id)}><TableCell className="font-mono text-xs">{String(i.email ?? '')}</TableCell><TableCell><Badge variant="outline">{String(i.role ?? '')}</Badge></TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(i.created_at)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(i.expires_at)}</TableCell><TableCell><Badge variant={st === 'pending' ? 'secondary' : st === 'accepted' ? 'default' : 'outline'}>{st}</Badge></TableCell><TableCell className="text-right">{st === 'pending' && <Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen('Revoke invite?', () => keep.run(() => raw('DELETE', `/v1/admin/operators/invites/${encodeURIComponent(String(i.id))}`), 'Invite revoked'))}>Revoke</Button>}</TableCell></TableRow> })}</TableBody></Table>
      </CardContent></Card>}
      {tab === 'sso' && <Card><CardHeader className="pb-2"><CardTitle className="text-base">Single sign-on (OIDC)</CardTitle><p className="text-sm text-muted-foreground">Operators from your identity provider get a console session with the role mapped from their groups; unknown groups fall back to the default role.</p></CardHeader><CardContent>
        <form className="grid max-w-2xl grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(e) => { e.preventDefault(); keep.run(() => raw('PUT', '/v1/admin/sso', { enabled: !!s.enabled, issuer: s.issuer, login_url: s.login_url, default_role: s.default_role ?? 'viewer', session_hours: Number(s.session_hours) || 12, roles: typeof s.roles === 'string' ? JSON.parse(String(s.roles) || '{}') : s.roles ?? {} }), 'SSO settings saved') }}>
          <label className="col-span-full flex items-center gap-2 text-sm"><Checkbox checked={!!s.enabled} onCheckedChange={(c) => setSsoF({ ...s, enabled: !!c })} />Enabled</label>
          <Field label="Issuer URL"><Input value={String(s.issuer ?? '')} onChange={(e) => setSsoF({ ...s, issuer: e.target.value })} placeholder="https://login.example.com/realms/studio" /></Field>
          <Field label="Login URL (shown on the sign-in page)"><Input value={String(s.login_url ?? '')} onChange={(e) => setSsoF({ ...s, login_url: e.target.value })} /></Field>
          <Field label="Default role"><Select value={String(s.default_role ?? 'viewer')} onValueChange={(v) => setSsoF({ ...s, default_role: v })}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent>{ROLES.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Session hours"><Input type="number" min={1} value={String(s.session_hours ?? 12)} onChange={(e) => setSsoF({ ...s, session_hours: e.target.value })} /></Field>
          <Field label="Group → role map (JSON)" hint='{"console-admins": "admin", "support": "operator"}'><Input className="font-mono" value={typeof s.roles === 'string' ? s.roles : JSON.stringify(s.roles ?? {})} onChange={(e) => setSsoF({ ...s, roles: e.target.value })} /></Field>
          <div className="col-span-full flex gap-2"><Button type="submit" size="sm" disabled={keep.pending}>Save</Button>{sso.data?.enabled || sso.data?.issuer ? <Button type="button" size="sm" variant="destructive" disabled={keep.pending} onClick={() => confirmThen('Remove SSO configuration? SSO operators keep their accounts but must use a password.', () => keep.run(() => raw('DELETE', '/v1/admin/sso'), 'SSO removed'))}>Remove</Button> : null}</div>
        </form>
      </CardContent></Card>}
      {tab === 'jwt' && <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">JWT signing keys</CardTitle><div className="flex gap-2"><Button size="sm" variant="outline" disabled={keep.pending} onClick={() => confirmThen('Rotate the signing key? New sessions use the new key; old tokens stay valid until they expire.', () => keep.run(() => raw('POST', '/v1/admin/jwt/rotate'), 'Signing key rotated'))}>Rotate</Button></div></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>kid</TableHead><TableHead>Algorithm</TableHead><TableHead>Created</TableHead><TableHead>Retired</TableHead><TableHead /></TableRow></TableHeader>
          <TableBody>{!keys.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">{jwt.isPending ? 'Loading…' : 'No keys — the node generates one at first operator login.'}</TableCell></TableRow> : keys.map((k) => <TableRow key={String(k.kid)}><TableCell className="font-mono text-xs">{String(k.kid)}{k.active || k.current ? <Badge className="ml-2">active</Badge> : null}</TableCell><TableCell className="text-xs">{String(k.alg ?? '')}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(k.created_at)}</TableCell><TableCell className="whitespace-nowrap text-xs text-muted-foreground">{ts(k.retired_at)}</TableCell><TableCell className="text-right">{!k.retired_at && <Button size="sm" variant="ghost" disabled={keep.pending} onClick={() => confirmThen(`Retire key ${String(k.kid)}? Tokens signed with it are rejected from now on.`, () => keep.run(() => raw('DELETE', `/v1/admin/jwt/keys/${encodeURIComponent(String(k.kid))}`), 'Key retired'))}>Retire</Button>}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>}
      <FormDialog open={dlg === 'add'} onOpenChange={(o) => !o && setDlg('')} title="Add operator" submit="Add" pending={pending} disabled={!f.email || String(f.password ?? '').length < 8} onSubmit={() => run(() => raw('POST', '/v1/admin/operators', { email: f.email, name: f.name || undefined, role: f.role, password: f.password }), 'Operator added — they sign in with e-mail and password')}>
        <Field label="E-mail"><Input type="email" value={String(f.email ?? '')} onChange={(e) => setF((x) => ({ ...x, email: e.target.value }))} /></Field>
        <Field label="Name"><Input value={String(f.name ?? '')} onChange={(e) => setF((x) => ({ ...x, name: e.target.value }))} /></Field>
        <Field label="Role"><Select value={String(f.role ?? 'operator')} onValueChange={(v) => setF((x) => ({ ...x, role: v }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent>{ROLES.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Initial password" hint="At least 8 characters; they can change it after signing in."><Input type="password" value={String(f.password ?? '')} onChange={(e) => setF((x) => ({ ...x, password: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'pw'} onOpenChange={(o) => !o && setDlg('')} title={`Reset password · ${String(f.email ?? '')}`} submit="Reset" destructive pending={pending} disabled={String(f.password ?? '').length < 8} onSubmit={() => run(() => raw('PUT', `/v1/admin/operators/${encodeURIComponent(String(f.id))}/password`, { password: f.password }), 'Password reset; their sessions were signed out')}>
        <Field label="New password"><Input type="password" value={String(f.password ?? '')} onChange={(e) => setF((x) => ({ ...x, password: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'invite'} onOpenChange={(o) => { if (!o) { setDlg(''); setMade(null) } }} title="Invite operator" submit={made ? 'Done' : 'Create invite'} pending={keep.pending} disabled={!made && !f.email} onSubmit={() => made ? setDlg('') : keep.run(async () => { setMade((await raw<Row>('POST', '/v1/admin/operators/invites', { email: f.email, role: f.role, expires_in_hours: Number(f.hours) || undefined })) ?? {}) }, 'Invite created')}>
        {made ? <div className="space-y-2"><p className="text-sm">Send this to the invitee — it is shown once. They open it, set a password and land in the console with the role <b>{String(f.role)}</b>.</p><JsonView value={made} /></div> : <>
          <Field label="E-mail"><Input type="email" value={String(f.email ?? '')} onChange={(e) => setF((x) => ({ ...x, email: e.target.value }))} /></Field>
          <Field label="Role"><Select value={String(f.role ?? 'operator')} onValueChange={(v) => setF((x) => ({ ...x, role: v }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent>{ROLES.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Expires in (hours)"><Input type="number" min={1} value={String(f.hours ?? '')} onChange={(e) => setF((x) => ({ ...x, hours: e.target.value }))} /></Field></>}
      </FormDialog>
    </div>
  )
}
