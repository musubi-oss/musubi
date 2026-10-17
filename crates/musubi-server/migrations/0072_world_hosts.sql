-- Fencing for world persistence (plan 4.4). A node may write snapshot / history / journal rows of
-- a world only while it holds a current term in world_hosts; the node that takes the world (or a
-- cell) over deletes the loser's row, so a host cut off from the quorum but still reaching
-- Postgres cannot overwrite the state the new host runs on.
create table if not exists musubi.world_hosts (
    name    text not null references musubi.worlds(name) on delete cascade,
    node_id uuid not null,
    term    bigint not null,
    since   timestamptz not null default now(),
    primary key (name, node_id)
);
alter table musubi.world_snapshots add column if not exists node_term bigint not null default 0;
alter table musubi.world_history   add column if not exists node_term bigint not null default 0;
alter table musubi.world_journal   add column if not exists node_term bigint not null default 0;
