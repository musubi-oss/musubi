-- Item definitions are versioned: every change bumps `version` and keeps the previous definition;
-- inventory entries remember the version they were granted under so instance data can be migrated.
alter table musubi.items
    add column if not exists version int not null default 1,
    add column if not exists instance_schema jsonb,
    add column if not exists migrations jsonb not null default '[]'::jsonb;

create table if not exists musubi.item_versions (
    item_id    text not null references musubi.items(id) on delete cascade,
    version    int not null,
    definition jsonb not null,
    note       text not null default '',
    created_at timestamptz not null default now(),
    primary key (item_id, version)
);

alter table musubi.inventory
    add column if not exists item_version int not null default 1;
create index if not exists inventory_item_version_idx on musubi.inventory (item_id, item_version);
