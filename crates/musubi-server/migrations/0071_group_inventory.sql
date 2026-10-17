create table if not exists musubi.group_inventory (
    group_id   uuid not null references musubi.groups(id) on delete cascade,
    item_id    text not null references musubi.items(id) on delete restrict,
    quantity   bigint not null check (quantity > 0),
    updated_at timestamptz not null default now(),
    primary key (group_id, item_id)
);

create table if not exists musubi.group_inventory_ledger (
    id             uuid primary key,
    group_id       uuid not null references musubi.groups(id) on delete cascade,
    account_id     uuid,
    item_id        text not null,
    delta          bigint not null,
    quantity_after bigint not null,
    reason         text not null default '',
    created_at     timestamptz not null default now()
);
create index if not exists group_inventory_ledger_group_idx
    on musubi.group_inventory_ledger (group_id, created_at desc);
