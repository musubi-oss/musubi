// A catalog `Act` as a button: optional confirm, optional editable JSON body, then the call.
import { useState } from 'react'
import { toast } from 'sonner'
import type { Act } from '@/model/catalog'
import { fill, type Row } from '@/model/resource'
import { raw } from '@/lib/api'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Textarea } from '@/components/ui/textarea'

export function ActionButton({ act, row, id, onDone, size = 'sm' }: { act: Act; row: Row | null; id?: string; onDone?: () => void; size?: 'sm' | 'default' }) {
  const [open, setOpen] = useState(false)
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  async function send(body?: unknown) {
    setBusy(true)
    try {
      await raw(act.method, fill(act.path, { ...(row ?? {}), ...((body as Row) ?? {}) }, id), body)
      toast.success(`${act.label}: ok`); setOpen(false); onDone?.()
    } catch (e) { toast.error(e instanceof Error ? e.message : String(e)) } finally { setBusy(false) }
  }
  function click() {
    if (act.body) { setText(JSON.stringify(act.body, null, 2)); setOpen(true); return }
    if (act.confirm && !window.confirm(act.confirm)) return
    void send()
  }
  function submit() {
    let body: unknown
    try { body = JSON.parse(text) } catch { toast.error('body is not valid JSON'); return }
    if (act.confirm && !window.confirm(act.confirm)) return
    void send(body)
  }
  return (
    <>
      <Button variant={act.danger ? 'destructive' : 'outline'} size={size} disabled={busy} onClick={click}>{act.label}</Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader><DialogTitle>{act.label}</DialogTitle><DialogDescription className="font-mono text-xs">{act.method} {fill(act.path, row, id)}</DialogDescription></DialogHeader>
          <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={10} className="font-mono text-xs" />
          <DialogFooter><Button variant="ghost" onClick={() => setOpen(false)}>Cancel</Button><Button variant={act.danger ? 'destructive' : 'default'} disabled={busy} onClick={submit}>Send</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
