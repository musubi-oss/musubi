alter table musubi.nodes add column if not exists raft_id bigint not null default 0;
