import { queryOptions } from '@tanstack/react-query'
import { api } from './api'

export const nodeQuery = queryOptions({
  queryKey: ['node'],
  queryFn: async () => {
    const r = await api.GET('/v1/node')
    if (r.error) throw r.error
    return r.data as { name?: string; roles?: string[]; region?: string; project?: string; environment?: string; version?: string }
  },
  staleTime: 60_000,
})
