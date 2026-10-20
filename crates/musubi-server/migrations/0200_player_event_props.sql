-- Per-player aggregates derived from every ingested telemetry event (Hiro/Satori computed
-- properties): count / first & last seen / first, high, low, last, sum of `properties.value`.
create table if not exists musubi.player_event_props (
    account_id      uuid not null references musubi.accounts(id) on delete cascade,
    event           text not null,
    count           bigint not null default 0,
    seen_first      timestamptz not null,
    seen_last       timestamptz not null,
    value_first     double precision,
    value_high      double precision,
    value_low       double precision,
    value_last      double precision,
    value_sum       double precision not null default 0,
    last_session_id text,
    updated_at      timestamptz not null default now(),
    primary key (account_id, event)
);

create index if not exists player_event_props_event_count_idx
    on musubi.player_event_props (event, count);
