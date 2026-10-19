-- Party directory: labels and listing (competitive P0-23). Rows are owned by the party's home
-- node, refreshed while the party lives and removed when it closes; readers ignore stale rows.
create table if not exists musubi.party_directory (
    party_id   text primary key,
    node_id    uuid not null,
    label      text not null default '',
    open       boolean not null default false,
    hidden     boolean not null default false,
    max_size   integer not null,
    size       integer not null,
    leader_id  uuid,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create index if not exists party_directory_label_idx on musubi.party_directory (label text_pattern_ops);
create index if not exists party_directory_node_idx on musubi.party_directory (node_id);
