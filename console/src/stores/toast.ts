import { reactive } from 'vue'
export const toast = reactive({ msg: '', bad: false, t: 0 as any,
  show(msg: string, bad = false) { this.msg = msg; this.bad = bad; clearTimeout(this.t); this.t = setTimeout(() => (this.msg = ''), 3500) },
  err(e: any) { this.show(e?.message ?? String(e), true) } })
