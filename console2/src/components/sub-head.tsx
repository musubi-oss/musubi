import type { ReactNode } from 'react'

export function SubHead({ title, count, children }: { title: string; count?: number; children?: ReactNode }) {
  return (
    <div className="mt-6 mb-2 flex flex-wrap items-center gap-2 first:mt-0">
      <h3 className="text-sm font-semibold">
        {title}
        {count !== undefined && <span className="ml-1.5 font-normal text-muted-foreground">({count})</span>}
      </h3>
      {children && <div className="ml-auto flex gap-2">{children}</div>}
    </div>
  )
}
