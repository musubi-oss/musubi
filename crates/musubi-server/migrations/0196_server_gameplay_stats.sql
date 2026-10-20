-- Server reputation (Steamworks GSReputation / GSGameplayStats parity): gameplay statistics a game
-- server reports about itself, combined with player ratings and crash reports into a score.
create table if not exists musubi.server_gameplay_stats (
    server_id       uuid primary key,
    fleet_id        uuid not null,
    rounds          bigint not null default 0,
    players_served  bigint not null default 0,
    play_secs       bigint not null default 0,
    kicks           bigint not null default 0,
    crashes         bigint not null default 0,
    reports         int not null default 0,
    updated_at      timestamptz not null default now()
);
create index if not exists server_gameplay_stats_fleet on musubi.server_gameplay_stats (fleet_id);
