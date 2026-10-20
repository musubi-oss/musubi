-- Sweepstakes / raffles (PlayFab Sweepstakes parity): players buy entries with currency, a
-- deterministic seeded draw picks `winners` distinct accounts weighted by entries.
create table if not exists musubi.sweepstakes (
    id                       text primary key,
    name                     text not null default '',
    description              text not null default '',
    entry_cost               jsonb not null default '{}'::jsonb,
    max_entries_per_player   int not null default 1,
    winners                  int not null default 1,
    prize_table              text not null,
    consolation_table        text,
    starts_at                timestamptz not null default now(),
    ends_at                  timestamptz not null,
    status                   text not null default 'open' check (status in ('open', 'drawn', 'cancelled')),
    enabled                  bool not null default true,
    metadata                 jsonb not null default '{}'::jsonb,
    seed                     text,
    drawn_at                 timestamptz,
    created_at               timestamptz not null default now(),
    updated_at               timestamptz not null default now()
);
create table if not exists musubi.sweepstake_entries (
    sweepstake_id  text not null references musubi.sweepstakes(id) on delete cascade,
    account_id     uuid not null,
    entries        int not null default 0,
    paid           jsonb not null default '{}'::jsonb,
    updated_at     timestamptz not null default now(),
    primary key (sweepstake_id, account_id)
);
create table if not exists musubi.sweepstake_winners (
    sweepstake_id  text not null references musubi.sweepstakes(id) on delete cascade,
    rank           int not null,
    account_id     uuid not null,
    rewards        jsonb not null default '[]'::jsonb,
    primary key (sweepstake_id, rank)
);
create index if not exists sweepstakes_due on musubi.sweepstakes (ends_at) where status = 'open';
