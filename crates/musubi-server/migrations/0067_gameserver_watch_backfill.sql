-- Game-server SDK depth (plan 5.9): WatchGameServer (a change counter the long-poll waits on) and
-- backfill slots (the server reports players / capacity; allocations may join a live server).
alter table musubi.game_servers
    add column if not exists players  int not null default 0,
    add column if not exists capacity int not null default 0,
    add column if not exists version  bigint not null default 1;
create or replace function musubi.game_servers_bump_version() returns trigger as $$
begin
    new.version := old.version + 1;
    return new;
end $$ language plpgsql;
drop trigger if exists game_servers_version on musubi.game_servers;
create trigger game_servers_version before update on musubi.game_servers
    for each row execute function musubi.game_servers_bump_version();
