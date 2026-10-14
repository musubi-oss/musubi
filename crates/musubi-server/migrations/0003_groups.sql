-- Groups (clans/guilds). Member states mirror Nakama: 0 superadmin, 1 admin, 2 member, 3 join request.
create table if not exists musubi.groups (
    id           uuid primary key,
    creator_id   uuid references musubi.accounts(id) on delete set null,
    name         text not null,
    description  text not null default '',
    avatar_url   text,
    lang         text not null default 'en',
    metadata     jsonb not null default '{}'::jsonb,
    open         boolean not null default true,
    edge_count   integer not null default 0,
    max_count    integer not null default 100 check (max_count between 1 and 10000),
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    unique (name)
);
create index if not exists groups_name_lower_idx on musubi.groups (lower(name) text_pattern_ops);
create index if not exists groups_open_edge_idx on musubi.groups (open, edge_count desc, id);

create table if not exists musubi.group_members (
    group_id    uuid not null references musubi.groups(id) on delete cascade,
    user_id     uuid not null references musubi.accounts(id) on delete cascade,
    state       smallint not null check (state between 0 and 3),
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    primary key (group_id, user_id)
);
create index if not exists group_members_user_idx on musubi.group_members (user_id, state);
create index if not exists group_members_group_state_idx on musubi.group_members (group_id, state, created_at);
