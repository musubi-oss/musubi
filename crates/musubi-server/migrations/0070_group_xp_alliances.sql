-- group progress and alliances (plan 5.4)
alter table musubi.groups
    add column if not exists xp    bigint not null default 0,
    add column if not exists level integer not null default 1;
create table if not exists musubi.alliances (
    id           uuid primary key,
    name         text not null unique,
    leader_group uuid not null references musubi.groups(id) on delete cascade,
    created_at   timestamptz not null default now()
);
create table if not exists musubi.alliance_members (
    alliance_id uuid not null references musubi.alliances(id) on delete cascade,
    group_id    uuid not null references musubi.groups(id) on delete cascade,
    state       smallint not null check (state between 0 and 1), -- 0 member, 1 invited
    joined_at   timestamptz not null default now(),
    primary key (alliance_id, group_id),
    unique (group_id)
);
