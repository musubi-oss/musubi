-- P0-53: live-event labels, explicit participation windows, templates
alter table musubi.live_events
    add column if not exists labels jsonb not null default '[]'::jsonb,
    add column if not exists participation text not null default 'implicit',
    add column if not exists participation_secs bigint,
    add column if not exists max_participants bigint,
    add column if not exists template text;

create table if not exists musubi.live_event_templates (
    id          text primary key,
    name        text not null,
    body        jsonb not null default '{}'::jsonb,
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.live_event_participants (
    event_id         text not null references musubi.live_events(id) on delete cascade,
    account_id       uuid not null,
    cycle_started_at bigint not null,
    joined_at        bigint not null,
    ends_at          bigint not null,
    primary key (event_id, account_id, cycle_started_at)
);
create index if not exists live_event_participants_account on musubi.live_event_participants (account_id);
