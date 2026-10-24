import { createFileRoute } from '@tanstack/react-router'
import { useMemo, useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

/** Chat word filters applied to every channel message (after leet-speak normalisation): mask replaces the word with asterisks, block rejects the message. The tester below runs the same rules locally on a sample line. */
export const Route = createFileRoute('/_app/social/chat_filters')({ component: ChatFilters })

const DEF = (): Row => ({ id: '', lang: '', pattern: '', regex: false, action: 'mask' })
const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
function compile(f: Row): RegExp | null { try { return new RegExp(f.regex ? String(f.pattern) : `\\b${esc(String(f.pattern))}\\b`, 'giu') } catch { return null } }
export function applyFilters(filters: Row[], text: string, lang?: string): { text: string; blocked: Row | null; masked: boolean } {
  let out = text; let masked = false
  for (const f of filters) {
    if (!f.pattern || (f.lang && lang && f.lang !== lang)) continue
    const re = compile(f); if (!re || !re.test(out)) continue; re.lastIndex = 0
    if (f.action === 'block') return { text, blocked: f, masked }
    out = out.replace(re, (m) => '*'.repeat(m.length)); masked = true
  }
  return { text: out, blocked: null, masked }
}

function ChatFilters() {
  const q = useSub<Row>('/v1/admin/chat/filters')
  const rows = listOf(q.data)
  const [open, setOpen] = useState(false); const [isNew, setIsNew] = useState(true); const [f, setF] = useState<Row>(DEF())
  const [sample, setSample] = useState('')
  const { run, pending } = useAct([['sub']], () => setOpen(false))
  const edit = (r?: Row) => { setIsNew(!r); setF(r ? { ...DEF(), ...r, lang: r.lang ?? '' } : DEF()); setOpen(true) }
  const res = useMemo(() => applyFilters(rows, sample), [rows, sample])
  const bad = !!f.regex && !!f.pattern && !compile(f)
  const trial = f.pattern && !bad ? applyFilters([f], sample || `hey ${f.regex ? 'test' : f.pattern} there`) : null
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Chat filters' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Chat filters</h1><p className="mt-1 text-sm text-muted-foreground">{rows.length} filters · a block filter rejects the whole message, mask filters star out the matched words</p></div>
        <Button size="sm" onClick={() => edit()}>Add filter</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <DataTable id="chat-filters" rows={rows} loading={q.isPending} empty="No filters — chat is delivered as typed"
          cols={[{ key: 'id', mono: true }, { key: 'pattern', mono: true, fmt: (r) => <span>{String(r.pattern)} <span className="text-xs text-muted-foreground">({r.regex ? 'regex' : 'word'})</span></span> }, { key: 'action', fmt: (r) => <Badge variant={r.action === 'block' ? 'destructive' : 'outline'}>{String(r.action)}</Badge> }, { key: 'lang', dim: true, fmt: (r) => r.lang || 'all languages' }]}
          actions={(r) => <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => edit(r)}>Edit</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete filter ${r.id}?`, () => run(() => raw('DELETE', `/v1/admin/chat/filters/${encodeURIComponent(String(r.id))}`), 'Deleted'))}>×</Button></span>} />
      </CardContent></Card>
      <Card><CardContent className="pt-6">
        <h3 className="mb-2 text-sm font-semibold">Try a message</h3>
        <Input value={sample} placeholder="type a chat line to see what players would receive" onChange={(e) => setSample(e.target.value)} />
        {sample && <p className="mt-2 text-sm">{res.blocked ? <span className="text-destructive">Blocked by <span className="font-mono">{String(res.blocked.id)}</span> — the sender gets an error, nobody receives it</span> : <span>Delivered as: <span className="font-mono">{res.text}</span>{res.masked ? <span className="ml-2 text-xs text-muted-foreground">(masked)</span> : null}</span>}</p>}
        <p className="mt-2 text-xs text-muted-foreground">The node also normalises leet-speak (h3ll0 → hello) before matching; this local preview does not.</p>
      </CardContent></Card>
      <FormDialog open={open} onOpenChange={setOpen} title={isNew ? 'New filter' : `Filter ${f.id}`} submit="Save" pending={pending} disabled={!f.id || !f.pattern || bad}
        onSubmit={() => run(() => raw('PUT', '/v1/admin/chat/filters', { id: f.id, lang: f.lang || null, pattern: f.pattern, regex: !!f.regex, action: f.action }), 'Saved')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Id"><Input className="font-mono" value={String(f.id)} disabled={!isNew} onChange={(e) => setF((x) => ({ ...x, id: e.target.value }))} /></Field>
          <Field label="Language (empty = all)"><Input className="font-mono" value={String(f.lang ?? '')} placeholder="en" onChange={(e) => setF((x) => ({ ...x, lang: e.target.value }))} /></Field>
        </div>
        <Field label={bad ? 'Pattern — invalid regular expression' : 'Pattern'}><Input className={`font-mono ${bad ? 'border-destructive' : ''}`} value={String(f.pattern)} onChange={(e) => setF((x) => ({ ...x, pattern: e.target.value }))} /></Field>
        <div className="grid grid-cols-2 items-end gap-3">
          <Field label="Action"><Select value={String(f.action)} onValueChange={(v) => setF((x) => ({ ...x, action: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="mask">mask — star out the word</SelectItem><SelectItem value="block">block — reject the message</SelectItem></SelectContent></Select></Field>
          <label className="flex items-center gap-2 pb-2 text-sm"><Checkbox checked={!!f.regex} onCheckedChange={(v) => setF((x) => ({ ...x, regex: !!v }))} /> Regular expression (otherwise whole word, case-insensitive)</label>
        </div>
        {trial && <p className="text-xs text-muted-foreground">Preview: «{trial.blocked ? 'blocked' : trial.text}»</p>}
      </FormDialog>
    </div>
  )
}
