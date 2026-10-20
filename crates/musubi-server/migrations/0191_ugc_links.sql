-- UGC dependency graph and collections (Steam Workshop parity): `dependency` = parent needs child
-- to work (mod → framework), `child` = parent is a collection listing child.
create table if not exists musubi.ugc_links (
    parent_id   uuid not null references musubi.ugc_items(id) on delete cascade,
    child_id    uuid not null references musubi.ugc_items(id) on delete cascade,
    rel         text not null check (rel in ('dependency', 'child')),
    position    int not null default 0,
    created_at  timestamptz not null default now(),
    primary key (parent_id, child_id, rel),
    check (parent_id <> child_id)
);
create index if not exists ugc_links_child_idx on musubi.ugc_links (child_id, rel);
