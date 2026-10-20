-- Per-process network accounting (Hathora egress parity): cumulative counters reported by the
-- game server, with the rate between two reports; rolled up per fleet with an egress price.
create table if not exists musubi.server_egress (
    server_id   uuid primary key,
    fleet_id    uuid not null,
    bytes_out   bigint not null default 0,
    bytes_in    bigint not null default 0,
    rate_out    double precision not null default 0,
    rate_in     double precision not null default 0,
    reports     int not null default 0,
    first_at    timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
create index if not exists server_egress_fleet_idx on musubi.server_egress (fleet_id);
