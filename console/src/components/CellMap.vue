<script setup lang="ts">
// Live map of a world's cells: one square per hot cell tinted by its owning simulator, cold
// (hibernated) cells dimmed, orphaned cells outlined red, entities as dots (amber while in
// transit between cells). Click a cell to pick it, hover for its summary.
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
export interface MapCell { x: number; y: number; owner?: string | null; cold?: boolean; orphaned?: boolean }
export interface MapEntity { x: number; y: number; transit?: boolean; cell?: { x: number; y: number } }
const props = defineProps<{ cells: MapCell[]; entities: MapEntity[]; cellSize: number; selected?: { x: number; y: number } | null; names?: Record<string, string> }>()
const emit = defineEmits<{ pick: [cell: MapCell] }>()
const el = ref<HTMLCanvasElement | null>(null); const wrap = ref<HTMLDivElement | null>(null)
const hover = ref<{ cell: MapCell; count: number; px: number; py: number } | null>(null)
const owners = computed(() => { const m = new Map<string, number>(); for (const c of props.cells) if (c.owner && !c.cold) m.set(c.owner, (m.get(c.owner) ?? 0) + 1); return [...m.entries()].sort((a, b) => b[1] - a[1]) })
// stable muted hue per simulator id, low saturation so the map stays calm
function hue(id: string) { let h = 0; for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0; return h % 360 }
const tint = (id: string, a = 0.28) => `hsla(${hue(id)},45%,55%,${a})`
const counts = computed(() => { const m = new Map<string, number>(); for (const e of props.entities) { const k = e.cell ? `${e.cell.x},${e.cell.y}` : `${Math.floor(e.x / props.cellSize)},${Math.floor(e.y / props.cellSize)}`; m.set(k, (m.get(k) ?? 0) + 1) } return m })
const box = computed(() => { const xs = props.cells.map((c) => c.x), ys = props.cells.map((c) => c.y); if (!xs.length) return { x0: 0, y0: 0, x1: 1, y1: 1 }; return { x0: Math.min(...xs), y0: Math.min(...ys), x1: Math.max(...xs) + 1, y1: Math.max(...ys) + 1 } })
let geom = { s: 1, ox: 0, oy: 0, w: 0, h: 0 }
function draw() {
  const cv = el.value, host = wrap.value; if (!cv || !host) return
  const dpr = window.devicePixelRatio || 1
  const w = host.clientWidth, b = box.value, nx = b.x1 - b.x0, ny = b.y1 - b.y0
  const s = Math.max(8, Math.min(Math.floor((w - 2) / nx), 160)); const h = Math.max(120, Math.min(ny * s + 2, 520))
  cv.width = w * dpr; cv.height = h * dpr; cv.style.width = w + 'px'; cv.style.height = h + 'px'
  const g = cv.getContext('2d')!; g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h)
  const ox = Math.floor((w - nx * s) / 2), oy = Math.floor((h - ny * s) / 2); geom = { s, ox, oy, w, h }
  const px = (x: number) => ox + (x - b.x0) * s, py = (y: number) => oy + (b.y1 - 1 - y) * s // y up
  for (const c of props.cells) {
    const x = px(c.x), y = py(c.y)
    g.fillStyle = c.cold ? 'rgba(107,107,120,.10)' : c.owner ? tint(c.owner) : 'rgba(160,160,171,.08)'
    g.fillRect(x, y, s, s)
    g.strokeStyle = c.orphaned ? '#ef5a5a' : '#2c2c34'; g.lineWidth = c.orphaned ? 1.5 : 1
    g.strokeRect(x + 0.5, y + 0.5, s - 1, s - 1)
    if (props.selected && props.selected.x === c.x && props.selected.y === c.y) { g.strokeStyle = '#4a85ff'; g.lineWidth = 2; g.strokeRect(x + 1, y + 1, s - 2, s - 2) }
    if (s >= 36) { g.fillStyle = c.cold ? '#6b6b78' : '#a0a0ab'; g.font = '10px ui-monospace,Menlo,monospace'; g.fillText(`${c.x},${c.y}`, x + 4, y + 12); const n = counts.value.get(`${c.x},${c.y}`); if (n && s >= 48) { g.fillStyle = '#6b6b78'; g.fillText(String(n), x + 4, y + s - 5) } }
  }
  const k = s / props.cellSize
  for (const e of props.entities) {
    const x = ox + (e.x / props.cellSize - b.x0) * k * props.cellSize, y = oy + (b.y1 - e.y / props.cellSize) * k * props.cellSize
    if (x < ox - 2 || y < oy - 2 || x > ox + nx * s + 2 || y > oy + ny * s + 2) continue
    g.fillStyle = e.transit ? '#f5b83d' : 'rgba(232,232,236,.75)'; g.fillRect(x - 1, y - 1, e.transit ? 3 : 2, e.transit ? 3 : 2)
  }
}
function at(ev: MouseEvent): MapCell | null { const r = el.value!.getBoundingClientRect(); const b = box.value; const cx = Math.floor((ev.clientX - r.left - geom.ox) / geom.s) + b.x0, cy = b.y1 - 1 - Math.floor((ev.clientY - r.top - geom.oy) / geom.s); return props.cells.find((c) => c.x === cx && c.y === cy) ?? null }
function move(ev: MouseEvent) { const c = at(ev); hover.value = c ? { cell: c, count: counts.value.get(`${c.x},${c.y}`) ?? 0, px: ev.offsetX, py: ev.offsetY } : null }
function click(ev: MouseEvent) { const c = at(ev); if (c) emit('pick', c) }
const name = (id?: string | null) => (id ? props.names?.[id] ?? '…' + id.slice(-8) : 'no simulator')
let ro: ResizeObserver | null = null
onMounted(() => { draw(); ro = new ResizeObserver(draw); if (wrap.value) ro.observe(wrap.value) })
onBeforeUnmount(() => ro?.disconnect())
watch(() => [props.cells, props.entities, props.selected, props.cellSize], draw, { deep: true })
</script>
<template>
  <div class="cellmap" ref="wrap">
    <canvas ref="el" @mousemove="move" @mouseleave="hover = null" @click="click"></canvas>
    <div class="tip" v-if="hover" :style="{ left: hover.px + 12 + 'px', top: hover.py + 12 + 'px' }">
      <b class="m">{{ hover.cell.x }},{{ hover.cell.y }}</b> · {{ hover.cell.cold ? 'hibernated' : hover.cell.orphaned ? 'orphaned — no simulator' : name(hover.cell.owner) }}<span v-if="!hover.cell.cold"> · {{ hover.count }} entities</span>
    </div>
    <div class="legend" v-if="cells.length">
      <span v-for="[o, n] in owners" :key="o"><i :style="{ background: tint(o, 0.9) }"></i><span class="m">{{ name(o) }}</span> <span class="dim">{{ n }} cells</span></span>
      <span v-if="cells.some((c) => c.cold)"><i style="background:rgba(107,107,120,.35)"></i><span class="dim">hibernated</span></span>
      <span v-if="cells.some((c) => c.orphaned)"><i style="border:1.5px solid #ef5a5a;background:transparent"></i><span class="dim">orphaned</span></span>
      <span v-if="entities.some((e) => e.transit)"><i style="background:#f5b83d;border-radius:50%"></i><span class="dim">in transit</span></span>
    </div>
    <div class="dim" v-else style="padding:30px;text-align:center">No hot cells — nothing is hosted on this node yet</div>
  </div>
</template>
<style scoped>
.cellmap{position:relative;border:1px solid var(--line);border-radius:8px;background:var(--bg2);overflow:hidden}
canvas{display:block;cursor:crosshair}
.tip{position:absolute;pointer-events:none;background:var(--card);border:1px solid var(--line2);border-radius:6px;padding:6px 9px;font-size:12px;white-space:nowrap;z-index:2}
.legend{display:flex;flex-wrap:wrap;gap:6px 16px;padding:8px 12px;border-top:1px solid var(--line);font-size:12px}
.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:6px;vertical-align:-1px}
</style>
