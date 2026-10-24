import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { ApiError } from '@/lib/api'

/** Run an operator action, toast the result and refresh every query under `keys`. */
export function useAct(keys: unknown[][], onDone?: () => void) {
  const qc = useQueryClient()
  const m = useMutation({
    mutationFn: async ({ fn }: { fn: () => Promise<unknown>; ok: string }) => fn(),
    onSuccess: (_d, v) => {
      toast.success(v.ok)
      for (const k of keys) void qc.invalidateQueries({ queryKey: k })
      onDone?.()
    },
    onError: (e) => toast.error(e instanceof ApiError ? `${e.status}: ${e.message}` : String(e)),
  })
  return { run: (fn: () => Promise<unknown>, ok: string) => m.mutate({ fn, ok }), pending: m.isPending }
}

export function confirmThen(msg: string, fn: () => void) {
  if (window.confirm(msg)) fn()
}
