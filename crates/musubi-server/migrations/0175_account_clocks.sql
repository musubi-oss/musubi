-- QA clocks (Satori parity): a tester's account can live at a simulated date. Live-ops reads
-- (live events, flag rule windows, offers, news, /v1/time) evaluate against that clock.
create table if not exists musubi.account_clocks (
    account_id  uuid primary key references musubi.accounts(id) on delete cascade,
    mode        text not null check (mode in ('fixed', 'offset')),
    at          bigint,
    offset_secs bigint,
    reason      text,
    expires_at  timestamptz,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
