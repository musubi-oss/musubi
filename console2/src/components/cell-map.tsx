import { useEffect, useMemo, useRef, useState } from 'react'

/** Live map of a world's cells: one square per hot cell tinted by its owning simulator, cold
 * (hibernated) cells dimmed, orphaned cells outlined red, entities as dots (amber while in
 * transit between cells). Click a cell to pick it, hover for its summary. */
export interface MapCell { x: number; y: number; owner?: string | null; cold?: boolean; orphaned?: boolean }
export interface MapEntity { x: number; y: number; transit?: boolean; cell?: { x: number; y: number } }

function hue(id: string) { let h = 0; for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0; return h % 360 }
const tint = (id: string, a = 0.28) => `hsla(${hue(id)},45%,55%,${a})`

export function CellMap({ cells, entities, cellSize, selected, names, onPick }: {
  cells: MapCell[]; entities: MapEntity[]; cellSize: number; selected?: { x: number; y: number } | null
  names?: Record<string, string>; onPick?: (c: MapCell) => void
}) {
  const wrap = useRef<HTMLDivElement>(null)
  const cv = useRef<HTMLCanvasElement>(null)
  const geom = useRef({ s: 1, ox: 0, oy: 0 })
  const [hover, setHover] = useState<{ cell: MapCell; count: number; px: number; py: number } | null>(null)
  const name = (id?: string | null) => (id ? (names?.[id] ?? '…' + id.slice(-8)) : 'no simulator')

  const owners = useMemo(() => { const m = new Map<string, number>(); for (const c of cells) if (c.owner && !c.cold) m.set(c.owner, (m.get(c.owner) ?? 0) + 1); return [...m.entries()].sort((a, b) => b[1] - a[1]) }, [cells])
  const counts = useMemo(() => { const m = new Map<string, number>(); for (const e of entities) { const k = e.cell ? `${e.cell.x},${e.cell.y}` : `${Math.floor(e.x / cellSize)},${Math.floor(e.y / cellSize)}`; m.set(k, (m.get(k) ?? 0) + 1) } return m }, [entities, cellSize])
  const box = useMemo(() => { const xs = cells.map((c) => c.x), ys = cells.map((c) => c.y); if (!xs.length) return { x0: 0, y0: 0, x1: 1, y1: 1 }; return { x0: Math.min(...xs), y0: Math.min(...ys), x1: Math.max(...xs) + 1, y1: Math.max(...ys) + 1 } }, [cells])

  useEffect(() => {
    const host = wrap.current, c = cv.current
    if (!host || !c) return
    const draw = () => {
      const dpr = window.devicePixelRatio || 1
      const w = host.clientWidth, b = box, nx = b.x1 - b.x0, ny = b.y1 - b.y0
      const s = Math.max(8, Math.min(Math.floor((w - 2) / nx), 160)); const h = Math.max(120, Math.min(ny * s + 2, 520))
      c.width = w * dpr; c.height = h * dpr; c.style.width = w + 'px'; c.style.height = h + 'px'
      const g = c.getContext('2d'); if (!g) return
      g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h)
      const ox = Math.floor((w - nx * s) / 2), oy = Math.floor((h - ny * s) / 2); geom.current = { s, ox, oy }
      const px = (x: number) => ox + (x - b.x0) * s, py = (y: number) => oy + (b.y1 - 1 - y) * s
      for (const cell of cells) {
        const x = px(cell.x), y = py(cell.y)
        g.fillStyle = cell.cold ? 'rgba(120,120,135,.10)' : cell.owner ? tint(cell.owner) : 'rgba(160,160,171,.08)'
        g.fillRect(x, y, s, s)
        g.strokeStyle = cell.orphaned ? '#ef5a5a' : 'rgba(128,128,140,.35)'; g.lineWidth = cell.orphaned ? 1.5 : 1
        g.strokeRect(x + 0.5, y + 0.5, s - 1, s - 1)
        if (selected && selected.x === cell.x && selected.y === cell.y) { g.strokeStyle = '#4a85ff'; g.lineWidth = 2; g.strokeRect(x + 1, y + 1, s - 2, s - 2) }
        if (s >= 36) {
          g.fillStyle = cell.cold ? 'rgba(128,128,140,.8)' : 'rgba(128,128,140,1)'; g.font = '10px ui-monospace,Menlo,monospace'; g.fillText(`${cell.x},${cell.y}`, x + 4, y + 12)
          const n = counts.get(`${cell.x},${cell.y}`); if (n && s >= 48) g.fillText(String(n), x + 4, y + s - 5)
        }
      }
      const k = s / cellSize
      for (const e of entities) {
        const x = ox + (e.x / cellSize - b.x0) * k * cellSize, y = oy + (b.y1 - e.y / cellSize) * k * cellSize
        if (x < ox - 2 || y < oy - 2 || x > ox + nx * s + 2 || y > oy + ny * s + 2) continue
        g.fillStyle = e.transit ? '#f5b83d' : 'rgba(128,128,140,.9)'; g.fillRect(x - 1, y - 1, e.transit ? 3 : 2, e.transit ? 3 : 2)
      }
    }
    draw()
    const ro = new ResizeObserver(draw); ro.observe(host)
    return () => ro.disconnect()
  }, [cells, entities, cellSize, selected, box, counts])

  function at(ev: React.MouseEvent<HTMLCanvasElement>): MapCell | null {
    const r = ev.currentTarget.getBoundingClientRect(); const { s, ox, oy } = geom.current
    const cx = Math.floor((ev.clientX - r.left - ox) / s) + box.x0, cy = box.y1 - 1 - Math.floor((ev.clientY - r.top - oy) / s)
    return cells.find((c) => c.x === cx && c.y === cy) ?? null
  }

  if (!cells.length) return <div className="rounded-md border p-8 text-center text-sm text-muted-foreground">No hot cells — nothing is hosted on this node yet</div>
  return (
    <div ref={wrap} className="relative w-full">
      <canvas ref={cv} className="block w-full cursor-crosshair rounded-md border bg-muted/20"
        onMouseMove={(ev) => { const c = at(ev); setHover(c ? { cell: c, count: counts.get(`${c.x},${c.y}`) ?? 0, px: ev.nativeEvent.offsetX, py: ev.nativeEvent.offsetY } : null) }}
        onMouseLeave={() => setHover(null)} onClick={(ev) => { const c = at(ev); if (c) onPick?.(c) }} />
      {hover && (
        <div className="pointer-events-none absolute z-10 rounded-md border bg-popover px-2 py-1 text-xs shadow-md" style={{ left: hover.px + 12, top: hover.py + 12 }}>
          <b className="font-mono">{hover.cell.x},{hover.cell.y}</b> · {hover.cell.cold ? 'hibernated' : hover.cell.orphaned ? 'orphaned — no simulator' : name(hover.cell.owner)}{!hover.cell.cold && <> · {hover.count} entities</>}
        </div>
      )}
      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        {owners.map(([o, n]) => <span key={o} className="inline-flex items-center gap-1.5"><i className="inline-block size-2.5 rounded-sm" style={{ background: tint(o, 0.9) }} /><span className="font-mono text-foreground">{name(o)}</span> {n} cells</span>)}
        {cells.some((c) => c.cold) && <span className="inline-flex items-center gap-1.5"><i className="inline-block size-2.5 rounded-sm" style={{ background: 'rgba(120,120,135,.35)' }} />hibernated</span>}
        {cells.some((c) => c.orphaned) && <span className="inline-flex items-center gap-1.5"><i className="inline-block size-2.5 rounded-sm border-[1.5px] border-[#ef5a5a]" />orphaned</span>}
        {entities.some((e) => e.transit) && <span className="inline-flex items-center gap-1.5"><i className="inline-block size-2.5 rounded-full" style={{ background: '#f5b83d' }} />in transit</span>}
      </div>
    </div>
  )
}
