import { useQuery } from '@tanstack/react-query'
import { ApiError, raw } from '@/lib/api'
import type { Row } from '@/model/resource'

/** One sub-resource of a detail page. 404 means "no data yet" and resolves to `null`. */
export function useSub<T = unknown>(path: string | null, enabled = true) {
  return useQuery({
    queryKey: ['sub', path],
    enabled: !!path && enabled,
    queryFn: async ({ signal }): Promise<T | null> => {
      try {
        return await raw<T>('GET', path as string, undefined, { signal })
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null
        throw e
      }
    },
  })
}

/** Rows out of a sub-resource payload: the array itself, `payload[sub]`, or the first array-valued field. */
export function listOf(v: unknown, sub?: string): Row[] {
  if (!v || typeof v !== 'object') return []
  const a = sub ? (v as Row)[sub] : Array.isArray(v) ? v : Object.values(v as Row).find(Array.isArray)
  return Array.isArray(a) ? (a as Row[]) : []
}
