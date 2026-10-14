-- Latest snapshot of every running authoritative match; a node that picks the row up can resume it.
create table if not exists musubi.match_snapshots (
    match_id    uuid primary key,
    module      text not null,
    label       text not null default '',
    params      jsonb not null default '{}'::jsonb,
    state       bytea not null,
    tick        bigint not null default 0,
    node_id     uuid,
    updated_at  timestamptz not null default now()
);
create index if not exists match_snapshots_node_idx on musubi.match_snapshots (node_id);
