import { Link } from '@tanstack/react-router'
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from '@/components/ui/breadcrumb'
import { Fragment } from 'react'

export function Crumbs({ items }: { items: { title: string; to?: string }[] }) {
  return (
    <Breadcrumb>
      <BreadcrumbList>
        {items.map((it, i) => (
          <Fragment key={i}>
            {i > 0 && <BreadcrumbSeparator />}
            <BreadcrumbItem>{it.to ? <BreadcrumbLink asChild><Link to={it.to}>{it.title}</Link></BreadcrumbLink> : <BreadcrumbPage className="max-w-[16rem] truncate">{it.title}</BreadcrumbPage>}</BreadcrumbItem>
          </Fragment>
        ))}
      </BreadcrumbList>
    </Breadcrumb>
  )
}
