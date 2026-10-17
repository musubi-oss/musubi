-- Fleet autoscale (plan 05): floor, scaling policy and per-fleet demand history (hour-of-week EWMA).
alter table musubi.fleets add column if not exists min_servers int not null default 0;
alter table musubi.fleets add column if not exists policy jsonb not null default '{}'::jsonb;

create table if not exists musubi.fleet_demand (
    fleet_id    uuid not null references musubi.fleets(id) on delete cascade,
    slot        smallint not null,          -- weekday*24 + hour (UTC), 0..167
    ewma        double precision not null default 0,  -- allocations per hour, smoothed over weeks
    samples     int not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (fleet_id, slot)
);
