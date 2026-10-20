-- Install scripts (GameLift install.sh parity): one row per fleet × target (node or host) ×
-- build version × install hash recording the last run of `spec.install.command`.
create table if not exists musubi.fleet_installs (
    fleet_id      uuid not null references musubi.fleets(id) on delete cascade,
    target        text not null,
    build_version text not null default '',
    install_hash  text not null,
    ok            boolean not null,
    exit_code     integer,
    output        text not null default '',
    duration_ms   bigint not null default 0,
    ran_at        timestamptz not null default now(),
    primary key (fleet_id, target, build_version, install_hash)
);
