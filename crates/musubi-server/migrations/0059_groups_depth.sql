-- Groups depth (plan 5.4): bans, announcements, event journal.
alter table musubi.groups add column if not exists announcement text;
alter table musubi.groups add column if not exists announcement_at timestamptz;

create table if not exists musubi.group_bans (
    group_id   uuid not null references musubi.groups(id) on delete cascade,
    user_id    uuid not null references musubi.accounts(id) on delete cascade,
    banned_by  uuid,
    reason     text not null default '',
    until      timestamptz,
    created_at timestamptz not null default now(),
    primary key (group_id, user_id)
);

create table if not exists musubi.group_events (
    id         uuid primary key,
    group_id   uuid not null references musubi.groups(id) on delete cascade,
    kind       text not null,
    actor_id   uuid,
    subject_id uuid,
    payload    jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);
create index if not exists group_events_group_idx on musubi.group_events (group_id, id desc);
