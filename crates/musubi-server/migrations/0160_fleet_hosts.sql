-- P1-11: dynamic host pool for the ssh provider + capacity providers (node-level autoscaling).
create table if not exists musubi.fleet_hosts (
    id             uuid primary key,
    name           text not null default '',
    addr           text not null,
    public_addr    text,
    ssh_user       text not null default 'musubi',
    ssh_port       integer not null default 22,
    ssh_key        text not null default '',
    capacity       integer not null default 8,
    region         text not null default '',
    class          text not null default '',
    labels         jsonb not null default '{}'::jsonb,
    state          text not null default 'ready',
    source         text not null default 'manual',
    pool           text not null default '',
    external_id    text,
    spot           boolean not null default false,
    cost_per_hour  double precision not null default 0,
    termination_at timestamptz,
    ready_at       timestamptz,
    last_probe_at  timestamptz,
    last_error     text,
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now()
);
create unique index if not exists fleet_hosts_addr_idx on musubi.fleet_hosts (addr) where state <> 'gone';
create index if not exists fleet_hosts_pool_idx on musubi.fleet_hosts (pool, state);
alter table musubi.game_servers add column if not exists host_id uuid;
create index if not exists game_servers_host_idx on musubi.game_servers (host_id) where host_id is not null;
