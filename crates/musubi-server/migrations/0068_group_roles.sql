-- role hierarchy with permission bitmasks (plan 5.4): ranks 0 (owner) .. 7, members carry a rank
create table if not exists musubi.group_roles (
    group_id    uuid not null references musubi.groups(id) on delete cascade,
    rank        smallint not null check (rank between 0 and 7),
    name        text not null,
    permissions bigint not null default 0,
    primary key (group_id, rank)
);
alter table musubi.group_members add column if not exists rank smallint not null default 2;
update musubi.group_members set rank = state where state < 2 and rank = 2;
