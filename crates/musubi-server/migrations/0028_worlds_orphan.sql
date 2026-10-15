-- A draining node disowns its worlds (node_id = null) so the failover leader re-homes them,
-- mirroring musubi.match_snapshots.
alter table musubi.worlds alter column node_id drop not null;

-- Several clusters may share one Postgres (node.cluster_group); failover must never adopt another
-- cluster's worlds or matches.
alter table musubi.worlds add column if not exists cluster_group text not null default 'default';
alter table musubi.match_snapshots add column if not exists cluster_group text not null default 'default';
