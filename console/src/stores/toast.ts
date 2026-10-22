import { reactive } from 'vue'
export interface Toast { id: number; msg: string; bad: boolean }
let seq = 0
export const toast = reactive({
  items: [] as Toast[],
  show(msg: string, bad = false) {
    const t = { id: ++seq, msg, bad }; this.items.push(t); if (this.items.length > 4) this.items.shift()
    setTimeout(() => this.dismiss(t.id), bad ? 6000 : 3000)
  },
  err(e: any) { this.show(e?.message ?? String(e), true) },
  dismiss(id: number) { const i = this.items.findIndex((t) => t.id === id); if (i >= 0) this.items.splice(i, 1) }
})
