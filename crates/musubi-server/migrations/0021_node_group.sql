-- Several independent Musubi clusters may share one Postgres: membership, bus and raft are scoped by group.
alter table musubi.nodes add column if not exists cluster_group text not null default 'default';
