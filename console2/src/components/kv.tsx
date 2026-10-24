import { Link } from '@tanstack/react-router'
import { Copy } from 'lucide-react'
import { toast } from 'sonner'
import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

export function KvGrid({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('grid gap-x-8 gap-y-1 sm:grid-cols-2 xl:grid-cols-3', className)}>{children}</div>
}

export function Kv({
  k,
  v,
  mono,
  copy,
  to,
}: {
  k: string
  v?: unknown
  mono?: boolean
  copy?: boolean
  to?: string | null
}) {
  const empty = v === undefined || v === null || v === ''
  const text = empty ? '—' : typeof v === 'object' ? JSON.stringify(v) : String(v)
  return (
    <div className="flex items-baseline gap-3 border-b border-border/50 py-1.5 text-sm">
      <dt className="w-32 shrink-0 text-muted-foreground">{k}</dt>
      <dd className={cn('min-w-0 flex-1 break-all', mono && 'font-mono text-xs', empty && 'text-muted-foreground')}>
        {to && !empty ? (
          <Link to={to} className="text-primary underline-offset-4 hover:underline">
            {text}
          </Link>
        ) : (
          text
        )}
        {copy && !empty && (
          <button
            type="button"
            aria-label={`Copy ${k}`}
            className="ml-2 inline-flex align-middle text-muted-foreground hover:text-foreground"
            onClick={() => void navigator.clipboard.writeText(text).then(() => toast.success('Copied'))}
          >
            <Copy className="size-3" />
          </button>
        )}
      </dd>
    </div>
  )
}
