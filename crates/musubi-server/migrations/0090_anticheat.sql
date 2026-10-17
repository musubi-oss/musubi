create table if not exists musubi.anticheat_signals (
    id          uuid primary key,
    account_id  uuid not null,
    session_id  uuid,
    match_id    text,
    source      text not null default 'server',
    kind        text not null,
    severity    integer not null,
    details     jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);
create index if not exists anticheat_signals_account on musubi.anticheat_signals (account_id, created_at desc);
create index if not exists anticheat_signals_recent on musubi.anticheat_signals (created_at desc);
