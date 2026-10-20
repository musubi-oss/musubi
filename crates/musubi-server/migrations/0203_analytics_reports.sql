-- Periodic analytics report snapshots (brainCloud report service parity).
create table if not exists musubi.analytics_reports (
    id            uuid primary key,
    kind          text not null check (kind in ('daily', 'weekly', 'adhoc')),
    for_date      date not null,
    period_days   integer not null,
    generated_at  timestamptz not null default now(),
    body          jsonb not null,
    unique (kind, for_date, period_days)
);

create index if not exists analytics_reports_kind_date_idx
    on musubi.analytics_reports (kind, for_date desc);
