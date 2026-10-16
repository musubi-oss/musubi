-- Boot epoch of a node: a process restarted under the same id registers a higher epoch, so
-- ownership rows written by its previous life can be reclaimed at once instead of waiting
-- for a heartbeat/SWIM verdict (which the restarted process keeps fresh).
alter table musubi.nodes add column if not exists epoch bigint not null default 0;
alter table musubi.worlds add column if not exists node_epoch bigint not null default 0;
alter table musubi.match_snapshots add column if not exists node_epoch bigint not null default 0;
