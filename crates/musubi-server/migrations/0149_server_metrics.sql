-- Per-server resource/tick metrics reported by the gameserver agent, fleet alert rules and alert state.
create table if not exists musubi.server_metrics (
    id             bigserial primary key,
    server_id      uuid not null,
    fleet_id       uuid not null,
    at             timestamptz not null default now(),
    cpu_pct        real,
    mem_bytes      bigint,
    mem_limit_bytes bigint,
    net_in_bps     bigint,
    net_out_bps    bigint,
    tick_ms        real,
    tick_p99_ms    real,
    tick_hz        real,
    players        integer,
    custom         jsonb not null default '{}'::jsonb
);
create index if not exists server_metrics_server_at_idx on musubi.server_metrics (server_id, at desc);
create index if not exists server_metrics_fleet_at_idx on musubi.server_metrics (fleet_id, at desc);

create table if not exists musubi.fleet_alert_rules (
    fleet_id    uuid not null references musubi.fleets(id) on delete cascade,
    name        text not null,
    metric      text not null,
    op          text not null check (op in ('gt', 'gte', 'lt', 'lte')),
    threshold   double precision not null,
    for_secs    integer not null default 0,
    severity    text not null default 'warning',
    enabled     boolean not null default true,
    updated_at  timestamptz not null default now(),
    primary key (fleet_id, name)
);

create table if not exists musubi.fleet_alerts (
    id            uuid primary key,
    fleet_id      uuid not null,
    server_id     uuid not null,
    rule          text not null,
    metric        text not null,
    severity      text not null,
    value         double precision,
    threshold     double precision not null,
    breach_since  timestamptz not null default now(),
    fired_at      timestamptz,
    resolved_at   timestamptz,
    last_value_at timestamptz not null default now()
);
create unique index if not exists fleet_alerts_open_idx on musubi.fleet_alerts (server_id, rule) where resolved_at is null;
create index if not exists fleet_alerts_fleet_idx on musubi.fleet_alerts (fleet_id, fired_at desc);
