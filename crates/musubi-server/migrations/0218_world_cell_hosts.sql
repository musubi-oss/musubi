-- Which node hosts a cell of a split world, for nodes without the raft directory (Postgres bus
-- only). There the assignment used to travel as one `pg_notify` and live only in the receiver's
-- cache: a listener reconnect lost it for good. The home writes this row before announcing;
-- a node that misses the announcement reads it on a cache miss and in its periodic reconcile.
create table if not exists musubi.world_cell_hosts (
    name       text     not null references musubi.worlds(name) on delete cascade,
    x          integer  not null,
    y          integer  not null,
    level      smallint not null default 0,
    node_id    uuid     not null,
    updated_at timestamptz not null default now(),
    primary key (name, x, y, level)
);
create index if not exists world_cell_hosts_node_idx on musubi.world_cell_hosts (node_id);
