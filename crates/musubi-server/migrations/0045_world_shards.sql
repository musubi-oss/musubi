-- Cells of a world may be hosted on several nodes: each host snapshots and journals its own
-- entities, keyed by node. Pre-existing rows belong to the zero node (legacy, whole-world).
alter table musubi.world_snapshots drop constraint world_snapshots_pkey;
alter table musubi.world_snapshots
    add column if not exists node_id uuid not null default '00000000-0000-0000-0000-000000000000';
alter table musubi.world_snapshots add primary key (name, node_id);
alter table musubi.world_journal add column if not exists node_id uuid;
