-- Team tournaments (plan 5.6): records are owned by groups, written by members holding `perm`;
-- team prizes are paid in currency into the group treasury instead of by mail.
create table if not exists musubi.tournament_team (
    tournament_id text primary key references musubi.leaderboards(id) on delete cascade,
    perm          bigint not null default 8
);
alter table musubi.tournament_prizes alter column reward_table set default '';
alter table musubi.tournament_prizes add column if not exists currency text;
alter table musubi.tournament_prizes add column if not exists amount bigint;
