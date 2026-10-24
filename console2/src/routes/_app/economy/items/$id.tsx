import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Item definition: every save is a new immutable version (instances keep the version they were minted with until migrated); status, lifecycle rules (cooldown, durability, effects), conversion to another item and revocation live here. `new` opens the create form. */
export const Route = createFileRoute('/_app/economy/items/$id')({ component: Item })

const ts = (v: unknown) => { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(String(v)); return isNaN(+d) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 19) }
const jsonOk = (s: string, allowEmpty = true) => { if (!s.trim()) return allowEmpty; try { JSON.parse(s); return true } catch { return false } }
const DEF: Row = { id: '', name: '', category: '', stackable: true, max_stack: 999, tradeable: true, unique: false, bind: 'none', properties: {}, slots: [] }

function Item() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate()
  const I = `/v1/server/items/${encodeURIComponent(id)}`
  const q = useSub<Row>(isNew ? null : I)
  const vers = useSub<Row>(isNew ? null : `${I}/versions`)
  const loaded = (q.data?.item ?? q.data) as Row | undefined
  const [draft, setDraft] = useState<{ key: string; it: Row; props: string; schema: string; slots: string } | null>(null)
  const key = `${id}:${String(loaded?.version ?? '')}`
  if (!draft || (draft.key !== key && (isNew || loaded))) {
    const it = isNew ? { ...DEF } : (loaded ?? DEF)
    setDraft({ key, it: { ...it }, props: JSON.stringify(it.properties ?? {}, null, 2), schema: it.instance_schema ? JSON.stringify(it.instance_schema, null, 2) : '', slots: ((it.slots ?? []) as string[]).join(', ') })
  }
  const it = draft?.it ?? DEF
  const setIt = (p: Row) => setDraft((d) => (d ? { ...d, it: { ...d.it, ...p } } : d))
  const [dlg, setDlg] = useState<'' | 'status' | 'lifecycle' | 'migrate' | 'convert' | 'revoke'>('')
  const [f, setF] = useState<Row>({})
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const propsOk = jsonOk(draft?.props ?? '{}'), schemaOk = jsonOk(draft?.schema ?? '')
  const save = () => run(async () => {
    const b = { ...it, id: it.id || undefined, properties: JSON.parse(draft?.props || '{}'), instance_schema: draft?.schema.trim() ? JSON.parse(draft.schema) : null, slots: (draft?.slots ?? '').split(',').map((s) => s.trim()).filter(Boolean), max_stack: Number(it.max_stack) }
    const r = (await raw<Row>('POST', '/v1/server/items', b)) as Row
    if (isNew) void nav({ to: '/economy/items/$id', params: { id: String(r.id ?? (r.item as Row | undefined)?.id ?? b.id) } })
  }, isNew ? 'Item created' : 'New item version saved')
  const effectsOk = jsonOk(String(f.effects ?? '[]'))
  const open = (k: typeof dlg, v: Row) => { setF(v); setDlg(k) }
  const lc = (it.lifecycle ?? {}) as Row
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Items', to: '/economy/items' }, { title: isNew ? 'New item' : String(loaded?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New item' : String(loaded?.name ?? id)}</h1>{!isNew && <div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{id}{loaded && <><Badge variant={loaded.status === 'active' || !loaded.status ? 'default' : 'secondary'}>{String(loaded.status ?? 'active')}</Badge><span>v{String(loaded.version ?? 1)}</span></>}</div>}</div>
        {!isNew && loaded && <>
          <Button size="sm" variant="outline" onClick={() => open('status', { status: loaded.status ?? 'active' })}>Status</Button>
          <Button size="sm" variant="outline" onClick={() => open('lifecycle', { use_cooldown_secs: lc.use_cooldown_secs ?? 0, dur_max: (lc.durability as Row | undefined)?.max ?? '', dur_per_use: (lc.durability as Row | undefined)?.per_use ?? 1, effects: JSON.stringify(lc.effects ?? [], null, 2) })}>Lifecycle rules</Button>
          <Button size="sm" variant="outline" onClick={() => open('migrate', { batch: 500 })}>Migrate instances</Button>
          <Button size="sm" variant="outline" onClick={() => open('convert', { to: '', ratio: 1, batch: '' })}>Convert</Button>
          <Button size="sm" variant="destructive" onClick={() => open('revoke', {})}>Revoke</Button>
        </>}
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="space-y-3 pt-6">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Id"><Input className="font-mono" value={String(it.id ?? '')} disabled={!isNew} placeholder="sword_iron" onChange={(e) => setIt({ id: e.target.value })} /></Field>
          <Field label="Name"><Input value={String(it.name ?? '')} onChange={(e) => setIt({ name: e.target.value })} /></Field>
          <Field label="Category"><Input value={String(it.category ?? '')} placeholder="weapon" onChange={(e) => setIt({ category: e.target.value })} /></Field>
          <Field label="Bind"><Select value={String(it.bind ?? 'none')} onValueChange={(v) => setIt({ bind: v })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="none">none</SelectItem><SelectItem value="pickup">on pickup</SelectItem><SelectItem value="equip">on equip</SelectItem></SelectContent></Select></Field>
          <Field label="Max stack"><Input type="number" value={String(it.max_stack ?? '')} disabled={!it.stackable} onChange={(e) => setIt({ max_stack: e.target.value })} /></Field>
          <Field label="Equip slots (comma separated)"><Input className="font-mono" value={draft?.slots ?? ''} placeholder="main_hand, off_hand" onChange={(e) => setDraft((d) => (d ? { ...d, slots: e.target.value } : d))} /></Field>
        </div>
        <div className="flex flex-wrap gap-4 text-sm">
          {(['stackable', 'tradeable', 'unique'] as const).map((k) => <label key={k} className="flex items-center gap-2"><Checkbox checked={!!it[k]} onCheckedChange={(v) => setIt({ [k]: !!v })} /> {k}</label>)}
        </div>
        <Field label={propsOk ? 'Properties (JSON — static stats shared by all instances)' : 'Properties — invalid JSON'}><Textarea className={`min-h-24 font-mono text-xs ${propsOk ? '' : 'border-destructive'}`} value={draft?.props ?? ''} onChange={(e) => setDraft((d) => (d ? { ...d, props: e.target.value } : d))} /></Field>
        <Field label={schemaOk ? 'Instance schema (JSON Schema for per-instance data, optional)' : 'Instance schema — invalid JSON'}><Textarea className={`min-h-16 font-mono text-xs ${schemaOk ? '' : 'border-destructive'}`} value={draft?.schema ?? ''} onChange={(e) => setDraft((d) => (d ? { ...d, schema: e.target.value } : d))} /></Field>
        <div className="flex items-center gap-3"><Button size="sm" disabled={pending || !it.id || !propsOk || !schemaOk} onClick={save}>{isNew ? 'Create' : 'Save as new version'}</Button>{!isNew && <span className="text-xs text-muted-foreground">Existing instances keep v{String(loaded?.version ?? 1)} until migrated.</span>}</div>
      </CardContent></Card>
      {!isNew && <Card><CardContent className="pt-6">
        <SubHead title="Versions" count={listOf(vers.data, 'versions').length} />
        <DataTable id="item-versions" rows={listOf(vers.data, 'versions')} loading={vers.isPending} empty="No version history" cols={[{ key: 'version', mono: true }, { key: 'created_at', label: 'created', fmt: (r) => ts(r.created_at), dim: true }, { key: 'name', dim: true }, { key: 'changes', fmt: (r) => (r.diff ? <JsonView value={r.diff} /> : r.properties ? <JsonView value={r.properties} /> : ''), dim: true }]}
          actions={(r) => Number(r.version) !== Number(loaded?.version) ? <Button size="sm" variant="ghost" onClick={() => confirmThen(`Roll back to v${r.version}? This creates a new version with that content.`, () => run(() => raw('POST', `${I}/versions/${r.version}/rollback`), 'Rolled back'))}>Roll back</Button> : <span className="text-xs text-muted-foreground">current</span>} />
      </CardContent></Card>}
      <FormDialog open={dlg === 'status'} onOpenChange={(o) => !o && setDlg('')} title="Item status" submit="Set" pending={pending} onSubmit={() => run(() => raw('POST', `${I}/status`, { status: f.status }), 'Status set')}>
        <Field label="Status"><Select value={String(f.status ?? 'active')} onValueChange={(v) => setF({ status: v })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{['active', 'deprecated', 'retired'].map((s) => <SelectItem key={s} value={s}>{s}</SelectItem>)}</SelectContent></Select></Field>
        <p className="text-xs text-muted-foreground">deprecated — no new grants or store sales, existing instances work; retired — instances are hidden from players too.</p>
      </FormDialog>
      <FormDialog open={dlg === 'lifecycle'} onOpenChange={(o) => !o && setDlg('')} title="Lifecycle rules" submit="Save rules" pending={pending} disabled={!effectsOk}
        onSubmit={() => run(() => raw('PUT', `${I}/lifecycle`, { use_cooldown_secs: Number(f.use_cooldown_secs) || 0, durability: f.dur_max === '' || f.dur_max == null ? null : { max: Number(f.dur_max), per_use: Number(f.dur_per_use) || 1 }, effects: JSON.parse(String(f.effects || '[]')) }), 'Lifecycle rules saved')}>
        <div className="grid grid-cols-3 gap-3">
          <Field label="Use cooldown (s)"><Input type="number" value={String(f.use_cooldown_secs ?? '')} onChange={(e) => setF((x) => ({ ...x, use_cooldown_secs: e.target.value }))} /></Field>
          <Field label="Durability max (empty = none)"><Input type="number" value={String(f.dur_max ?? '')} onChange={(e) => setF((x) => ({ ...x, dur_max: e.target.value }))} /></Field>
          <Field label="Durability per use"><Input type="number" value={String(f.dur_per_use ?? '')} onChange={(e) => setF((x) => ({ ...x, dur_per_use: e.target.value }))} /></Field>
        </div>
        <Field label={effectsOk ? 'Effects on use (JSON: [{key, value, duration_secs}])' : 'Effects — invalid JSON'}><Textarea className={`min-h-20 font-mono text-xs ${effectsOk ? '' : 'border-destructive'}`} value={String(f.effects ?? '')} onChange={(e) => setF((x) => ({ ...x, effects: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'migrate'} onOpenChange={(o) => !o && setDlg('')} title="Migrate instances to the current version" submit="Migrate" pending={pending}
        onSubmit={() => run(() => raw('POST', `${I}/migrate?batch=${Number(f.batch) || 500}`), 'Migration started')}>
        <p className="text-sm text-muted-foreground">Rewrites every instance still on an older version to v{String(loaded?.version ?? 1)}, in batches; players see the new stats on next load.</p>
        <Field label="Batch size"><Input type="number" value={String(f.batch ?? '')} onChange={(e) => setF((x) => ({ ...x, batch: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'convert'} onOpenChange={(o) => !o && setDlg('')} title="Convert to another item" submit="Convert" pending={pending} disabled={!f.to}
        onSubmit={() => confirmThen(`Replace every ${id} with ${f.to} at ${f.ratio}:1?`, () => run(() => raw('POST', `${I}/convert`, { to: f.to, ratio: Number(f.ratio) || 1, batch: Number(f.batch) || undefined }), 'Converted'))}>
        <p className="text-sm text-muted-foreground">Swaps all player-held instances for another item — the way to retire a currency-like item without deleting anyone's inventory.</p>
        <div className="grid grid-cols-3 gap-3"><div className="col-span-2"><Field label="Target item id"><Input className="font-mono" value={String(f.to ?? '')} onChange={(e) => setF((x) => ({ ...x, to: e.target.value }))} /></Field></div><Field label="Ratio (old per new)"><Input type="number" min={1} value={String(f.ratio ?? '')} onChange={(e) => setF((x) => ({ ...x, ratio: e.target.value }))} /></Field></div>
        <Field label="Batch size (optional)"><Input type="number" value={String(f.batch ?? '')} onChange={(e) => setF((x) => ({ ...x, batch: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'revoke'} onOpenChange={(o) => !o && setDlg('')} title={`Revoke ${id}`} submit="Revoke everywhere" pending={pending}
        onSubmit={() => run(() => raw('POST', `${I}/revoke`), 'Revoked')}>
        <p className="text-sm text-destructive">Removes every instance from every inventory and marks the item retired. Irreversible — for exploit cleanup.</p>
      </FormDialog>
    </div>
  )
}
