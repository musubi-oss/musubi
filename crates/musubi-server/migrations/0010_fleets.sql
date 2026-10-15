-- Dedicated-server orchestration: fleets (what to run, how many) and game servers (instances).
create table if not exists musubi.fleets (
    id            uuid primary key,
    name          text not null unique,
    provider      text not null default 'local',
    region        text not null default '',
    spec          jsonb not null default '{}'::jsonb,
    buffer        int not null default 0,
    max_servers   int not null default 1,
    port_start    int not null default 7777,
    port_end      int not null default 7877,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

create table if not exists musubi.game_servers (
    id            uuid primary key,
    fleet_id      uuid not null references musubi.fleets(id) on delete cascade,
    state         text not null default 'scheduled',
    node_id       uuid,
    address       text not null default '',
    port          int not null default 0,
    pid           bigint,
    labels        jsonb not null default '{}'::jsonb,
    allocation    jsonb,
    connect_token text,
    last_health   timestamptz not null default now(),
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);
create index if not exists game_servers_fleet_state_idx on musubi.game_servers (fleet_id, state);
create index if not exists game_servers_node_idx on musubi.game_servers (node_id);
