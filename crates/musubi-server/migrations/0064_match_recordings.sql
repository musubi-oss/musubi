-- Match recording (plan 5.8): matches created with params.record = true keep every snapshot and
-- journal chunk for replay (the failover journal in match_journal is pruned at each snapshot).
create table if not exists musubi.match_recordings (
    match_id   uuid primary key,
    module     text not null,
    label      text not null default '',
    params     jsonb not null default '{}'::jsonb,
    started_at timestamptz not null default now(),
    ended_at   timestamptz,
    last_tick  bigint not null default 0
);
create table if not exists musubi.match_recording_frames (
    match_id   uuid not null references musubi.match_recordings(match_id) on delete cascade,
    kind       text not null check (kind in ('snapshot', 'journal')),
    tick_from  bigint not null,
    tick_to    bigint not null,
    state      bytea,
    entries    jsonb,
    created_at timestamptz not null default now(),
    primary key (match_id, kind, tick_from)
);
create index if not exists match_recordings_started_idx on musubi.match_recordings (started_at);
