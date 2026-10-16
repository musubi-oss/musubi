-- journal chunks are per shard (node): home and cell hosts tick independently, so (name, tick_from)
-- collided across nodes and chunks overwrote each other
update musubi.world_journal set node_id = '00000000-0000-0000-0000-000000000000' where node_id is null;
alter table musubi.world_journal alter column node_id set not null;
alter table musubi.world_journal drop constraint if exists world_journal_pkey;
alter table musubi.world_journal add primary key (name, node_id, tick_from);
