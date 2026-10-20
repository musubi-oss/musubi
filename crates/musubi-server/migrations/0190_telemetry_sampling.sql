-- Title-wide telemetry sampling policy (Pragma parity): a global keep-percentage multiplied into
-- every event schema's own `sampling_pct`, plus events that are never sampled out.
create table if not exists musubi.telemetry_sampling (
    id              int primary key default 1 check (id = 1),
    global_pct      int not null default 100 check (global_pct between 0 and 100),
    always_enabled  text[] not null default '{}',
    updated_at      timestamptz not null default now()
);
insert into musubi.telemetry_sampling (id) values (1) on conflict do nothing;
