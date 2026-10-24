import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { qs, raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'

/** Remote config document: versioned JSON value, per-segment variants (selector + patch), history with diff / rollback, optional external source. */
export const Route = createFileRoute('/_app/liveops/configs/$name')({ component: ConfigPage })

function ConfigPage() {
  const { name } = Route.useParams()
  const nav = useNavigate({ from: Route.fullPath })
  const C = `/v1/server/configs/${encodeURIComponent(name)}`
  const q = useQuery({ queryKey: ['config', name], queryFn: async ({ signal }) => { const r = await raw<Row>('GET', C, undefined, { signal }); return (r.config ?? r) as Row } })
  const history = useSub(`${C}/history`); const variants = useSub(`${C}/variants`); const source = useSub<Row>(`${C}/source`)
  const c = q.data
  const [text, setText] = useState(''); const [dirty, setDirty] = useState(false)
  useEffect(() => { if (c) { setText(JSON.stringify(c.value ?? {}, null, 2)); setDirty(false) } }, [c])
  const [diffFrom, setDiffFrom] = useState<string | null>(null)
  const diffQ = useQuery({ queryKey: ['config-diff', name, diffFrom], enabled: diffFrom !== null, queryFn: ({ signal }) => raw('GET', `${C}/diff` + qs({ from: diffFrom }), undefined, { signal }) })
  const [vdlg, setVdlg] = useState(false)
  const [vf, setVf] = useState<Row>({ variant: '', priority: 1, enabled: true, selector: '{\n  "countries": []\n}', patch: '{}' })
  const { run, pending } = useAct([['config', name], ['sub']], () => setVdlg(false))
  function saveValue() { let value: unknown; try { value = JSON.parse(text) } catch { return toast.error('Value is not valid JSON') }; run(() => raw('PUT', C, { value }), `Saved as version ${Number(c?.version ?? 0) + 1}`) }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Remote config', to: '/liveops/configs' }, { title: name }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate font-mono text-xl font-semibold">{name}</h1><p className="mt-1 text-xs text-muted-foreground">Clients fetch the value (plus the first matching variant's patch) by name; every save is a new version you can diff and roll back to.</p></div>
        <Button size="sm" variant="outline" onClick={() => setVdlg(true)}>Add variant</Button>
        {source.data && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${C}/source/sync`), 'Synced from source')}>Sync source</Button>}
        <Button size="sm" variant="destructive" onClick={() => confirmThen(`Delete config ${name}?`, () => run(async () => { await raw('DELETE', C); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'configs' } }) }, 'Deleted'))}>Delete</Button>
        <Button size="sm" disabled={!c || !dirty || pending} onClick={saveValue}>Save value</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {c && (
        <Card><CardContent className="pt-6">
          <KvGrid><Kv k="Version" v={c.version} /><Kv k="Checksum" v={c.checksum} mono copy /><Kv k="Updated" v={ts(c.updated_at)} /><Kv k="Variants" v={listOf(variants.data).length} /><Kv k="Source" v={source.data ? `${source.data.kind ?? 'external'} · ${source.data.url ?? source.data.path ?? ''}` : 'none (edited here)'} /></KvGrid>
          <SubHead title="Value"><span className="self-center text-xs text-muted-foreground">JSON — saving creates version {Number(c.version ?? 0) + 1}</span></SubHead>
          <Textarea className="min-h-64 font-mono text-xs" value={text} onChange={(e) => { setText(e.target.value); setDirty(true) }} aria-label="config value" />
          <SubHead title="Variants" count={listOf(variants.data).length} />
          <DataTable id="config-variants" tools={false} rows={listOf(variants.data)} loading={variants.isPending} empty="No variants — everyone gets the base value"
            cols={[{ key: 'variant', mono: true }, { key: 'priority' }, { key: 'enabled', fmt: (r) => (r.enabled === false ? 'off' : 'on') }, { key: 'selector', dim: true, fmt: (r) => JSON.stringify(r.selector ?? {}) }, { key: 'patch', dim: true, fmt: (r) => JSON.stringify(r.patch ?? {}) }]}
            actions={(r) => <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Delete variant?', () => run(() => raw('DELETE', `${C}/variants/${encodeURIComponent(String(r.variant))}`), 'Variant deleted'))}>Delete</Button>} />
          <SubHead title="History" count={listOf(history.data).length} />
          <DataTable id="config-history" tools={false} rows={listOf(history.data)} loading={history.isPending} empty="No history yet"
            cols={[{ key: 'version' }, { key: 'id', dim: true }, { key: 'checksum', mono: true, dim: true, fmt: (r) => String(r.checksum ?? '').slice(0, 12) }, { key: 'note', dim: true }, { key: 'deleted', fmt: (r) => (r.deleted ? 'yes' : '') }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]}
            actions={(r) => <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => setDiffFrom(String(r.id))}>Diff</Button>{r.version !== c.version && <Button size="sm" variant="ghost" onClick={() => confirmThen(`Roll back to version ${r.version}?`, () => run(() => raw('POST', `${C}/rollback`, { to: r.id, version: r.version }), 'Rolled back'))}>Rollback</Button>}</span>} />
          {diffFrom !== null && <><SubHead title="Diff"><span className="self-center text-xs text-muted-foreground">history #{diffFrom} → current</span></SubHead><JsonView value={diffQ.data} empty="No differences" /></>}
        </CardContent></Card>
      )}
      <FormDialog open={vdlg} onOpenChange={setVdlg} title="Add variant" submit="Add" pending={pending} disabled={!vf.variant}
        onSubmit={() => { let selector: unknown, patch: unknown; try { selector = JSON.parse(String(vf.selector || '{}')); patch = JSON.parse(String(vf.patch || '{}')) } catch { return toast.error('Selector / patch must be valid JSON') }; run(() => raw('POST', `${C}/variants`, { variant: vf.variant, priority: Number(vf.priority), enabled: !!vf.enabled, selector, patch }), 'Variant added') }}>
        <div className="grid grid-cols-2 gap-3"><Field label="Variant name"><Input value={String(vf.variant)} onChange={(e) => setVf((f) => ({ ...f, variant: e.target.value }))} /></Field><Field label="Priority (lower wins)"><Input type="number" value={String(vf.priority)} onChange={(e) => setVf((f) => ({ ...f, priority: e.target.value }))} /></Field></div>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!vf.enabled} onCheckedChange={(v) => setVf((f) => ({ ...f, enabled: v === true }))} /> enabled</label>
        <Field label="Selector (JSON: countries, platforms, audience, min_version…)"><Textarea className="min-h-24 font-mono text-xs" value={String(vf.selector)} onChange={(e) => setVf((f) => ({ ...f, selector: e.target.value }))} /></Field>
        <Field label="Patch (JSON merged over the base value)"><Textarea className="min-h-24 font-mono text-xs" value={String(vf.patch)} onChange={(e) => setVf((f) => ({ ...f, patch: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
